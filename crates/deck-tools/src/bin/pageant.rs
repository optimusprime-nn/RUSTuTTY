//! Pageant: holds keys for PuTTY, RUSTuTTY and other SSH programs, answering
//! on the window PuTTY's clients look for.
//!
//!   pageant [key.ppk ...] [-c command [arguments]]
//!
//! As PuTTY's, it sits in the notification area: its menu views and adds
//! keys, and opens sessions. Passphrases are asked for in a box of their own.
//! Run again with keys while it's running, it hands them to the one running.
//! `-c` runs a command once the keys are in. Exiting forgets every key.

#![windows_subsystem = "windows"]

use anyhow::{anyhow, bail, Context};
use deck_core::config::load_sessions;
use deck_core::ppk::Ppk;
use deck_tools::gui::{self, control, place, wide};
use russh::keys::agent::client::AgentClient;
use russh::keys::ssh_key::HashAlg;
use russh::keys::{load_secret_key, PrivateKey, PublicKey};
use std::cell::RefCell;
use std::sync::OnceLock;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
use windows::Win32::Security::{EqualSid, GetTokenInformation, TokenUser, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW};
use windows::Win32::UI::WindowsAndMessaging::*;

// from PuTTY's windows/pageant.c and pageant.h
const AGENT_COPYDATA_ID: usize = 0x804e50ba;
const AGENT_MAX_MSGLEN: usize = 262144;

/// The notification area's messages to the window.
const WM_TRAY: u32 = WM_APP + 1;

// the menu, as PuTTY's Pageant has it
const IDM_VIEW: usize = 0x10;
const IDM_ADD: usize = 0x20;
const IDM_NEW: usize = 0x30;
const IDM_ABOUT: usize = 0x40;
const IDM_HELP: usize = 0x50;
const IDM_EXIT: usize = 0x60;
const IDM_SAVED: usize = 0x1000;

// the key list window's controls
const KEYS_LIST: usize = 1;
const KEYS_ADD: usize = 2;
const KEYS_REMOVE: usize = 3;
const KEYS_CLOSE: usize = 4;

struct Server {
    runtime: Runtime,
    connect: UnboundedSender<DuplexStream>,
}

static SERVER: OnceLock<Server> = OnceLock::new();

/// Each key's comment, by its public key's bytes. russh's agent keeps no
/// comments, and PuTTY's does, so they're kept here and put back into the
/// key lists that programs ask for.
static COMMENTS: std::sync::Mutex<Vec<(Vec<u8>, String)>> = std::sync::Mutex::new(Vec::new());

fn comment_for(blob: &[u8]) -> Option<String> {
    COMMENTS.lock().ok()?.iter().find(|(b, _)| b == blob).map(|(_, c)| c.clone())
}

/// An SSH string at the start of `data`, and what follows it.
fn string(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let n = u32::from_be_bytes(data.get(..4)?.try_into().ok()?) as usize;
    Some((data.get(4..4 + n)?, &data[4 + n..]))
}

/// The agent's list of keys (SSH2_AGENT_IDENTITIES_ANSWER), with each key's
/// comment filled in.
fn with_comments(reply: Vec<u8>) -> Vec<u8> {
    let fill = || -> Option<Vec<u8>> {
        if *reply.get(4)? != 12 {
            return None;
        }
        let count = u32::from_be_bytes(reply.get(5..9)?.try_into().ok()?);
        let mut rest = &reply[9..];
        let mut body = vec![12u8];
        body.extend(count.to_be_bytes());
        for _ in 0..count {
            let (blob, after) = string(rest)?;
            let (comment, after) = string(after)?;
            let comment = comment_for(blob).map_or(comment.to_vec(), String::into_bytes);
            body.extend((blob.len() as u32).to_be_bytes());
            body.extend(blob);
            body.extend((comment.len() as u32).to_be_bytes());
            body.extend(comment);
            rest = after;
        }
        let mut out = (body.len() as u32).to_be_bytes().to_vec();
        out.extend(body);
        Some(out)
    };
    fill().unwrap_or(reply)
}

thread_local! {
    /// The key list window, while it's open.
    static KEYS: RefCell<Option<HWND>> = const { RefCell::new(None) };
    /// Saved sessions, by their place in the menu.
    static SAVED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// A request to add a key, with its comment, which russh's own client leaves
/// out (SSH2_AGENTC_ADD_IDENTITY).
fn add_message(key: &PrivateKey) -> anyhow::Result<Vec<u8>> {
    use russh::keys::ssh_encoding::Encode;
    let mut m = vec![0, 0, 0, 0, 17];
    key.key_data().encode(&mut m)?;
    key.comment().encode(&mut m)?;
    let n = (m.len() - 4) as u32;
    m[..4].copy_from_slice(&n.to_be_bytes());
    Ok(m)
}

/// Keeps the comment of a key being added, from whichever program adds it.
fn note_comment(request: &[u8]) {
    use russh::keys::ssh_encoding::{Decode, Encode};
    use russh::keys::ssh_key::private::KeypairData;
    use russh::keys::ssh_key::public::KeyData;
    if !matches!(request.get(4), Some(17 | 25)) {
        return;
    }
    let mut r = &request[5..];
    let Ok(pair) = KeypairData::decode(&mut r) else { return };
    let Ok(comment) = String::decode(&mut r) else { return };
    let Ok(public) = KeyData::try_from(&pair) else { return };
    let mut blob = Vec::new();
    if public.encode(&mut blob).is_ok() {
        if let Ok(mut all) = COMMENTS.lock() {
            all.retain(|(b, _)| *b != blob);
            all.push((blob, comment));
        }
    }
}

/// The agent's answer to one request, over a private connection to it.
fn ask(server: &Server, request: Vec<u8>) -> Option<Vec<u8>> {
    note_comment(&request);
    let (mut near, far) = tokio::io::duplex(AGENT_MAX_MSGLEN);
    server.connect.send(far).ok()?;
    server.runtime.block_on(async move {
        near.write_all(&request).await.ok()?;
        let mut len = [0u8; 4];
        near.read_exact(&mut len).await.ok()?;
        let n = u32::from_be_bytes(len) as usize;
        if n > AGENT_MAX_MSGLEN - 4 {
            return None;
        }
        let mut reply = vec![0u8; n + 4];
        reply[..4].copy_from_slice(&len);
        near.read_exact(&mut reply[4..]).await.ok()?;
        Some(with_comments(reply))
    })
}

/// A client of the agent held here, for the menu's own work.
fn agent<T>(work: impl AsyncFnOnce(&mut AgentClient<DuplexStream>) -> T) -> Option<T> {
    let server = SERVER.get()?;
    let (near, far) = tokio::io::duplex(AGENT_MAX_MSGLEN);
    server.connect.send(far).ok()?;
    let mut client = AgentClient::connect(near);
    Some(server.runtime.block_on(work(&mut client)))
}

fn own_sid() -> Option<Vec<u8>> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut len = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        let got = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len);
        let _ = CloseHandle(token);
        got.ok()?;
        Some(buf)
    }
}

/// Serves a client's request in its shared memory. The memory has to belong to
/// this user: otherwise another account could use the keys held here.
unsafe fn serve_mapping(name: &[u16]) -> bool {
    let Some(server) = SERVER.get() else { return false };
    let Ok(map) = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, PCWSTR(name.as_ptr())) else { return false };
    let mut ok = false;
    let mut owner = PSID::default();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let found = GetSecurityInfo(map, SE_KERNEL_OBJECT, OWNER_SECURITY_INFORMATION, Some(&mut owner), None, None, None, Some(&mut descriptor));
    if let (true, Some(sid)) = (found.is_ok(), own_sid()) {
        let user = &*(sid.as_ptr() as *const TOKEN_USER);
        if EqualSid(owner, user.User.Sid).is_ok() {
            let view = MapViewOfFile(map, FILE_MAP_ALL_ACCESS, 0, 0, 0);
            if !view.Value.is_null() {
                let mut info = MEMORY_BASIC_INFORMATION::default();
                VirtualQuery(Some(view.Value), &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>());
                let size = info.RegionSize.min(AGENT_MAX_MSGLEN);
                let memory = std::slice::from_raw_parts_mut(view.Value as *mut u8, size);
                if size >= 4 {
                    let n = u32::from_be_bytes([memory[0], memory[1], memory[2], memory[3]]) as usize;
                    if n + 4 <= size {
                        if let Some(reply) = ask(server, memory[..n + 4].to_vec()) {
                            if reply.len() <= size {
                                memory[..reply.len()].copy_from_slice(&reply);
                                ok = true;
                            }
                        }
                    }
                }
                let _ = UnmapViewOfFile(view);
            }
        }
    }
    if !descriptor.0.is_null() {
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
    }
    let _ = CloseHandle(map);
    ok
}

/// A key file, asking for its passphrase in a box when it has one. `None` is
/// a Cancel.
fn load(path: &str, owner: Option<HWND>) -> anyhow::Result<Option<PrivateKey>> {
    let text = std::fs::read(path).with_context(|| format!("reading {path}"))?;
    if !text.starts_with(b"PuTTY-User-Key-File") {
        return match load_secret_key(path, None) {
            Err(russh::keys::Error::KeyIsEncrypted) => match gui::passphrase(owner, "Pageant: Loading Encrypted Key", &format!("Enter passphrase for {path}:"), false) {
                Some(pass) => Ok(Some(load_secret_key(path, Some(&pass)).map_err(|e| anyhow!("{e}"))?)),
                None => Ok(None),
            },
            other => Ok(Some(other.map_err(|e| anyhow!("{e}"))?)),
        };
    }
    let file = Ppk::parse(&String::from_utf8_lossy(&text))?;
    if !file.encrypted {
        return file.to_private_key("").map(|mut k| {
            k.set_comment(file.comment.as_str());
            Some(k)
        });
    }
    loop {
        let Some(pass) = gui::passphrase(owner, "Pageant: Loading Encrypted Key", &format!("Enter passphrase for key {}:", file.comment), false) else { return Ok(None) };
        match file.to_private_key(&pass) {
            Err(e) if e.to_string() == "wrong passphrase" => {}
            other => {
                return other.map(|mut k| {
                    k.set_comment(file.comment.as_str());
                    Some(k)
                })
            }
        }
    }
}

/// Loads a key file into the agent, saying in a box if it can't.
fn add_file(path: &str, owner: Option<HWND>) {
    let result = load(path, owner).and_then(|key| match key {
        Some(key) => {
            let reply = ask(SERVER.get().ok_or_else(|| anyhow!("Pageant has stopped"))?, add_message(&key)?);
            // SSH_AGENT_SUCCESS
            anyhow::ensure!(reply.as_deref().and_then(|r| r.get(4)) == Some(&6), "the agent refused the key");
            Ok(())
        }
        None => Ok(()),
    });
    if let Err(e) = result {
        deck_ui::tell(owner, "Pageant", &format!("Couldn't load {path}: {e:#}"), deck_ui::Tone::Error);
    }
    refresh_keys();
}

/// The keys held, as PuTTY's list shows them: type, fingerprint, comment.
fn held() -> Vec<(PublicKey, String)> {
    agent(async |a| a.request_identities().await.unwrap_or_default())
        .unwrap_or_default()
        .into_iter()
        .map(|id| {
            let key = id.public_key().into_owned();
            let comment = key.to_bytes().ok().and_then(|b| comment_for(&b)).unwrap_or_else(|| id.comment().to_string());
            let line = format!("{}  {}  {comment}", key.algorithm(), key.fingerprint(HashAlg::Sha256));
            (key, line)
        })
        .collect()
}

fn refresh_keys() {
    let Some(window) = KEYS.with(|k| *k.borrow()) else { return };
    unsafe {
        let Ok(list) = GetDlgItem(Some(window), KEYS_LIST as i32) else { return };
        SendMessageW(list, LB_RESETCONTENT, None, None);
        for (_, line) in held() {
            let t = wide(&line);
            SendMessageW(list, LB_ADDSTRING, None, Some(LPARAM(t.as_ptr() as isize)));
        }
    }
}

fn add_from_picker(owner: Option<HWND>) {
    if let Some(path) = gui::open_file(owner, "Select Private Key File", gui::KEY_FILES) {
        add_file(&path.to_string_lossy(), owner);
    }
}

unsafe extern "system" fn keys_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if let Some(r) = gui::themed(hwnd, msg, wp, lp) {
        return r;
    }
    match msg {
        WM_SIZE => {
            let mut r = RECT::default();
            let _ = GetClientRect(hwnd, &mut r);
            let p = |v| gui::px(hwnd, v);
            let (gap, bw, bh) = (p(10), p(100), p(28));
            let item = |id: usize| GetDlgItem(Some(hwnd), id as i32).unwrap_or_default();
            let y = r.bottom - gap - bh;
            place(item(KEYS_LIST), gap, gap, r.right - gap * 2, y - gap * 2);
            place(item(KEYS_ADD), gap, y, bw, bh);
            place(item(KEYS_REMOVE), gap * 2 + bw, y, bw, bh);
            place(item(KEYS_CLOSE), r.right - gap - bw, y, bw, bh);
        }
        WM_COMMAND => match wp.0 & 0xffff {
            KEYS_ADD => add_from_picker(Some(hwnd)),
            KEYS_REMOVE => {
                let list = GetDlgItem(Some(hwnd), KEYS_LIST as i32).unwrap_or_default();
                let keys = held();
                let n = SendMessageW(list, LB_GETSELCOUNT, None, None).0.max(0) as usize;
                let mut chosen = vec![0i32; n];
                SendMessageW(list, LB_GETSELITEMS, Some(WPARAM(n)), Some(LPARAM(chosen.as_mut_ptr() as isize)));
                for i in chosen {
                    if let Some((key, _)) = keys.get(i as usize) {
                        let _ = agent(async |a| a.remove_identity(key).await);
                    }
                }
                refresh_keys();
            }
            KEYS_CLOSE => {
                let _ = DestroyWindow(hwnd);
            }
            _ => {}
        },
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
        }
        WM_DESTROY => KEYS.with(|k| *k.borrow_mut() = None),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

/// PuTTY's Pageant Key List: the keys held, to add to and remove from.
fn view_keys() {
    unsafe {
        if let Some(open) = KEYS.with(|k| *k.borrow()) {
            let _ = ShowWindow(open, SW_RESTORE);
            let _ = SetForegroundWindow(open);
            return;
        }
        let Ok(h) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("PageantKeys"), w!("Pageant Key List"), WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN, CW_USEDEFAULT, CW_USEDEFAULT, 640, 360, None, None, Some(gui::instance()), None) else { return };
        deck_ui::style_window(h);
        let f = gui::font(h);
        let list_style = WS_TABSTOP | WS_VSCROLL | WS_HSCROLL | WINDOW_STYLE((LBS_EXTENDEDSEL | LBS_NOINTEGRALHEIGHT | LBS_NOTIFY) as u32);
        control(h, w!("LISTBOX"), "", list_style, WS_EX_CLIENTEDGE, KEYS_LIST, f);
        control(h, w!("BUTTON"), "&Add Key", WS_TABSTOP, WINDOW_EX_STYLE(0), KEYS_ADD, f);
        control(h, w!("BUTTON"), "&Remove Key", WS_TABSTOP, WINDOW_EX_STYLE(0), KEYS_REMOVE, f);
        control(h, w!("BUTTON"), "&Close", WS_TABSTOP, WINDOW_EX_STYLE(0), KEYS_CLOSE, f);
        let _ = SetWindowPos(h, None, 0, 0, gui::px(h, 640), gui::px(h, 360), SWP_NOMOVE | SWP_NOZORDER);
        KEYS.with(|k| *k.borrow_mut() = Some(h));
        refresh_keys();
        let _ = ShowWindow(h, SW_SHOW);
        let _ = SetForegroundWindow(h);
    }
}

/// RUSTuTTY, next to this program, for New Session and Saved Sessions.
fn start_rustutty(args: &[&str]) {
    let exe = std::env::current_exe().map(|p| p.with_file_name("rustutty.exe")).unwrap_or_default();
    if let Err(e) = std::process::Command::new(&exe).args(args).spawn() {
        deck_ui::tell(None, "Pageant", &format!("Couldn't start {}: {e}", exe.display()), deck_ui::Tone::Error);
    }
}

fn menu(hwnd: HWND) {
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(m, MF_STRING, IDM_VIEW, w!("&View Keys"));
        let _ = AppendMenuW(m, MF_STRING, IDM_ADD, w!("Add &Key"));
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(m, MF_STRING, IDM_NEW, w!("&New Session"));
        let names: Vec<String> = load_sessions().into_iter().map(|(n, _)| n).collect();
        if let Ok(saved) = CreatePopupMenu() {
            for (i, name) in names.iter().enumerate() {
                let t = wide(name);
                let _ = AppendMenuW(saved, MF_STRING, IDM_SAVED + i, PCWSTR(t.as_ptr()));
            }
            let flags = if names.is_empty() { MF_POPUP | MF_GRAYED } else { MF_POPUP };
            let _ = AppendMenuW(m, flags, saved.0 as usize, w!("&Saved Sessions"));
        }
        SAVED.with(|s| *s.borrow_mut() = names);
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(m, MF_STRING, IDM_ABOUT, w!("&About"));
        let _ = AppendMenuW(m, MF_STRING, IDM_HELP, w!("&Help"));
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(m, MF_STRING, IDM_EXIT, w!("E&xit"));
        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        // the menu goes away properly only if the window is in front
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(m, TPM_RIGHTBUTTON, at.x, at.y, None, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(m);
    }
}

fn tray(hwnd: HWND, add: bool) {
    // tests run Pageant with no icon, so nothing turns up on the taskbar
    if std::env::var_os("RUSTUTTY_PAGEANT_NO_TRAY").is_some() {
        return;
    }
    unsafe {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: LoadIconW(Some(gui::instance()), PCWSTR(1 as *const u16)).unwrap_or_default(),
            ..Default::default()
        };
        for (slot, c) in data.szTip.iter_mut().zip("Pageant".encode_utf16()) {
            *slot = c;
        }
        let _ = Shell_NotifyIconW(if add { NIM_ADD } else { NIM_DELETE }, &data);
    }
}

/// Explorer says this when it restarts, and the icon has to be put back.
fn taskbar_created() -> u32 {
    static ID: OnceLock<u32> = OnceLock::new();
    *ID.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

unsafe extern "system" fn window(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COPYDATA => {
            let cds = &*(lp.0 as *const COPYDATASTRUCT);
            if cds.dwData != AGENT_COPYDATA_ID || cds.cbData == 0 || cds.lpData.is_null() {
                return LRESULT(0);
            }
            let bytes = std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
            if bytes.last() != Some(&0) {
                return LRESULT(0);
            }
            let name: Vec<u16> = bytes.iter().map(|&b| b as u16).collect();
            return LRESULT(serve_mapping(&name) as isize);
        }
        WM_TRAY => match (lp.0 & 0xffff) as u32 {
            WM_LBUTTONDBLCLK => view_keys(),
            WM_RBUTTONUP | WM_LBUTTONUP => menu(hwnd),
            _ => {}
        },
        WM_COMMAND => match wp.0 & 0xffff {
            IDM_VIEW => view_keys(),
            IDM_ADD => add_from_picker(None),
            IDM_NEW => start_rustutty(&[]),
            IDM_ABOUT => deck_ui::tell(None, "About Pageant", concat!("Pageant, from RUSTuTTY ", env!("CARGO_PKG_VERSION"), "\n\nPuTTY's SSH agent rewritten in Rust.\nAn unofficial program, not connected with the PuTTY project."), deck_ui::Tone::Info),
            IDM_HELP => {
                let _ = ShellExecuteW(None, w!("open"), w!("https://optimusprime-nn.github.io/RUSTuTTY/Tools.html#pageant"), None, None, SW_SHOWNORMAL);
            }
            IDM_EXIT => {
                tray(hwnd, false);
                PostQuitMessage(0);
            }
            n if n >= IDM_SAVED => {
                if let Some(name) = SAVED.with(|s| s.borrow().get(n - IDM_SAVED).cloned()) {
                    start_rustutty(&["-load", &name]);
                }
            }
            _ => {}
        },
        m if m == taskbar_created() => tray(hwnd, true),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

/// Keys given to a second Pageant go to the one already running, as PuTTY's do.
fn hand_over(paths: &[String]) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    for path in paths {
        let Some(key) = load(path, None)? else { continue };
        let message = add_message(&key)?;
        runtime.block_on(async {
            let mut stream = pageant::wmmessage::PageantStream::new().await.map_err(|e| anyhow!("{e}"))?;
            stream.write_all(&message).await?;
            let mut reply = [0u8; 5];
            stream.read_exact(&mut reply).await?;
            anyhow::ensure!(reply[4] == 6, "the running Pageant refused the key");
            anyhow::Ok(())
        })?;
    }
    Ok(())
}

fn run() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // -c: a command to run once the keys are in, with the rest as its arguments
    let command = args.iter().position(|a| a == "-c").map(|i| args.split_off(i)[1..].to_vec());
    gui::sharp();
    deck_ui::init(true);
    unsafe {
        if FindWindowW(w!("Pageant"), w!("Pageant")).is_ok() {
            if args.is_empty() {
                bail!("Pageant is already running.");
            }
            return hand_over(&args);
        }
    }
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build()?;
    let (tx, mut rx) = unbounded_channel::<DuplexStream>();
    let incoming = futures::stream::poll_fn(move |cx| rx.poll_recv(cx).map(|s| s.map(Ok::<_, std::io::Error>)));
    runtime.spawn(russh::keys::agent::server::serve(Box::pin(incoming), ()));
    SERVER.set(Server { runtime, connect: tx }).map_err(|_| anyhow!("already started"))?;

    unsafe {
        let instance = gui::instance();
        let arrow = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let icon = LoadIconW(Some(instance), PCWSTR(1 as *const u16)).unwrap_or_default();
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(window), hInstance: instance, lpszClassName: w!("Pageant"), ..Default::default() });
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(keys_proc), hInstance: instance, hIcon: icon, hCursor: arrow, lpszClassName: w!("PageantKeys"), ..Default::default() });
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), w!("Pageant"), w!("Pageant"), WINDOW_STYLE(0), 0, 0, 0, 0, None, None, Some(instance), None)?;
        tray(hwnd, true);
        for path in &args {
            add_file(path, None);
        }
        if let Some(c) = command.filter(|c| !c.is_empty()) {
            if let Err(e) = std::process::Command::new(&c[0]).args(&c[1..]).spawn() {
                deck_ui::tell(None, "Pageant", &format!("Couldn't run {}: {e}", c[0]), deck_ui::Tone::Error);
            }
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let keys = KEYS.with(|k| *k.borrow());
            if keys.is_some_and(|k| IsDialogMessageW(k, &msg).as_bool()) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        tray(hwnd, false);
    }
    Ok(())
}

fn main() {
    // with no console, a failure is said in a box
    if let Err(e) = run() {
        if std::env::var_os("RUSTUTTY_PAGEANT_NO_TRAY").is_some() {
            eprintln!("{e:#}");
        } else {
            deck_ui::tell(None, "Pageant", &format!("{e:#}"), deck_ui::Tone::Error);
        }
        std::process::exit(1);
    }
}
