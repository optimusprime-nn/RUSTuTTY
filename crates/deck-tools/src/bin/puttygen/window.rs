//! PuTTY Key Generator, the window: makes a key or loads one, shows its
//! public key for pasting into authorized_keys, and saves it as a PuTTY key,
//! an OpenSSH key or a public key file. Laid out as PuTTYgen's is: the key at
//! the top, then the actions, then the parameters for a new key.

use deck_core::ppk::{self, Ppk};
use deck_tools::gui::{self, control, place, set_text, text_of, wide};
use russh::keys::ssh_key::{HashAlg, LineEnding};
use russh::keys::{load_secret_key, PrivateKey};
use std::cell::RefCell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Console::{FreeConsole, GetConsoleProcessList, GetConsoleWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// A key made on its own thread is ready.
const WM_MADE: u32 = WM_APP + 1;

// the controls
const STATUS: usize = 1;
const PUBLIC_LABEL: usize = 2;
const PUBLIC: usize = 3;
const PRINT_LABEL: usize = 4;
const PRINT: usize = 5;
const COMMENT_LABEL: usize = 6;
const COMMENT: usize = 7;
const PASS_LABEL: usize = 8;
const PASS: usize = 9;
const AGAIN_LABEL: usize = 10;
const AGAIN: usize = 11;
const GENERATE_LABEL: usize = 12;
const GENERATE: usize = 13;
const LOAD_LABEL: usize = 14;
const LOAD: usize = 15;
const SAVE_LABEL: usize = 16;
const SAVE_PUBLIC: usize = 17;
const SAVE_PRIVATE: usize = 18;
const TYPE_LABEL: usize = 19;
const RSA: usize = 20;
const ECDSA: usize = 21;
const EDDSA: usize = 22;
const BITS_LABEL: usize = 23;
const BITS: usize = 24;

// the menus
const IDM_EXPORT_OPENSSH: usize = 0x100;
const IDM_EXPORT_OPENSSH_NEW: usize = 0x101;
const IDM_IMPORT: usize = 0x102;
const IDM_EXIT: usize = 0x103;
const IDM_ABOUT: usize = 0x104;
const IDM_HELP: usize = 0x105;

thread_local! {
    /// The key shown, if there is one.
    static KEY: RefCell<Option<PrivateKey>> = const { RefCell::new(None) };
    /// A key being made on another thread.
    static MAKING: RefCell<Option<std::thread::JoinHandle<anyhow::Result<PrivateKey>>>> = const { RefCell::new(None) };
}

fn item(h: HWND, id: usize) -> HWND {
    unsafe { GetDlgItem(Some(h), id as i32).unwrap_or_default() }
}

fn checked(h: HWND, id: usize) -> bool {
    unsafe { SendMessageW(item(h, id), BM_GETCHECK, None, None).0 == 1 }
}

fn say(h: HWND, text: &str, tone: deck_ui::Tone) {
    deck_ui::tell(Some(h), "PuTTY Key Generator", text, tone);
}

/// Shows a key, or "No key." for none, and turns the controls that need one
/// on or off.
fn show(h: HWND, key: Option<&PrivateKey>, pass: &str) {
    let public = key.and_then(|k| k.public_key().to_openssh().ok()).unwrap_or_default();
    let print = key.map(|k| format!("{} {}", k.algorithm(), k.fingerprint(HashAlg::Sha256))).unwrap_or_default();
    set_text(item(h, STATUS), if key.is_some() { "Key" } else { "No key." });
    set_text(item(h, PUBLIC), &public);
    set_text(item(h, PRINT), &print);
    set_text(item(h, COMMENT), &key.map(|k| k.comment().to_string()).unwrap_or_default());
    set_text(item(h, PASS), pass);
    set_text(item(h, AGAIN), pass);
    // only the boxes go grey: a greyed label is drawn over itself unerased,
    // and smears
    for id in [PUBLIC, PRINT, COMMENT, PASS, AGAIN, SAVE_PUBLIC, SAVE_PRIVATE] {
        unsafe {
            let _ = EnableWindow(item(h, id), key.is_some());
        }
    }
    KEY.with(|k| *k.borrow_mut() = key.cloned());
}

/// The key with the comment typed in the box.
fn current(h: HWND) -> Option<PrivateKey> {
    let mut key = KEY.with(|k| k.borrow().clone())?;
    key.set_comment(text_of(item(h, COMMENT)).as_str());
    Some(key)
}

/// The passphrase typed, if it was typed the same twice.
fn passphrase(h: HWND) -> Option<String> {
    let (pass, again) = (text_of(item(h, PASS)), text_of(item(h, AGAIN)));
    if pass != again {
        say(h, "The two passphrases given do not match.", deck_ui::Tone::Error);
        return None;
    }
    if pass.is_empty() && !deck_ui::ask(Some(h), "PuTTY Key Generator", "Are you sure you want to save this key without a passphrase to protect it?", deck_ui::Tone::Warning) {
        return None;
    }
    Some(pass)
}

/// Starts making a key of the chosen type and size on its own thread.
fn generate(h: HWND) {
    let kind = if checked(h, RSA) {
        "rsa"
    } else if checked(h, ECDSA) {
        "ecdsa"
    } else {
        "ed25519"
    };
    let bits: usize = text_of(item(h, BITS)).trim().parse().unwrap_or(0);
    set_text(item(h, STATUS), "Generating key...");
    for id in [GENERATE, LOAD] {
        unsafe {
            let _ = EnableWindow(item(h, id), false);
        }
    }
    let target = h.0 as isize;
    let kind = kind.to_string();
    let job = std::thread::spawn(move || {
        let made = super::generate(&kind, bits);
        unsafe {
            let _ = PostMessageW(Some(HWND(target as *mut _)), WM_MADE, WPARAM(0), LPARAM(0));
        }
        made
    });
    MAKING.with(|m| *m.borrow_mut() = Some(job));
}

fn made(h: HWND) {
    let Some(job) = MAKING.with(|m| m.borrow_mut().take()) else { return };
    for id in [GENERATE, LOAD] {
        unsafe {
            let _ = EnableWindow(item(h, id), true);
        }
    }
    match job.join() {
        Ok(Ok(mut key)) => {
            // PuTTYgen's default comment: the type and the date, as
            // ed25519-key-20261103
            let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
            let kind = match key.algorithm() {
                russh::keys::Algorithm::Rsa { .. } => "rsa",
                russh::keys::Algorithm::Ecdsa { .. } => "ecdsa",
                _ => "ed25519",
            };
            key.set_comment(format!("{kind}-key-{:04}{:02}{:02}", t.wYear, t.wMonth, t.wDay).as_str());
            show(h, Some(&key), "");
        }
        Ok(Err(e)) => {
            show(h, None, "");
            say(h, &format!("Couldn't make the key: {e:#}"), deck_ui::Tone::Error);
        }
        Err(_) => show(h, None, ""),
    }
}

/// A key file of PuTTY's or OpenSSH's, asking for its passphrase if it has one.
fn load_file(h: HWND, path: &str) {
    let text = match std::fs::read(path) {
        Ok(t) => t,
        Err(e) => return say(h, &format!("Couldn't load {path}: {e}"), deck_ui::Tone::Error),
    };
    let loaded = if text.starts_with(b"PuTTY-User-Key-File") {
        Ppk::parse(&String::from_utf8_lossy(&text)).and_then(|file| {
            let mut pass = String::new();
            loop {
                if file.encrypted {
                    match gui::passphrase(Some(h), "PuTTYgen: Enter Passphrase", &format!("Enter passphrase for key {}:", file.comment), false) {
                        Some(p) => pass = p,
                        None => return Ok(None),
                    }
                }
                match file.to_private_key(&pass) {
                    Err(e) if e.to_string() == "wrong passphrase" => continue,
                    other => {
                        return other.map(|mut k| {
                            k.set_comment(file.comment.as_str());
                            Some((k, pass))
                        })
                    }
                }
            }
        })
    } else {
        match load_secret_key(path, None) {
            Ok(k) => Ok(Some((k, String::new()))),
            Err(russh::keys::Error::KeyIsEncrypted) => match gui::passphrase(Some(h), "PuTTYgen: Enter Passphrase", "Enter passphrase for key:", false) {
                Some(pass) => load_secret_key(path, Some(&pass)).map(|k| Some((k, pass))).map_err(|e| anyhow::anyhow!("{e}")),
                None => Ok(None),
            },
            Err(e) => Err(anyhow::anyhow!("{e}")),
        }
    };
    match loaded {
        Ok(Some((key, pass))) => show(h, Some(&key), &pass),
        Ok(None) => {}
        Err(e) => say(h, &format!("Couldn't load {path}: {e:#}"), deck_ui::Tone::Error),
    }
}

/// The public key as PuTTYgen saves it: the SSH-2 public key file of RFC 4716.
fn rfc4716(key: &PrivateKey) -> anyhow::Result<String> {
    let line = key.public_key().to_openssh().map_err(|e| anyhow::anyhow!("{e}"))?;
    let base64 = line.split_whitespace().nth(1).unwrap_or_default();
    let mut out = format!("---- BEGIN SSH2 PUBLIC KEY ----\r\nComment: \"{}\"\r\n", key.comment().to_string().replace('"', "\\\""));
    for chunk in base64.as_bytes().chunks(64) {
        out += &String::from_utf8_lossy(chunk);
        out += "\r\n";
    }
    out += "---- END SSH2 PUBLIC KEY ----\r\n";
    Ok(out)
}

fn write(h: HWND, path: &std::path::Path, text: &str) {
    if let Err(e) = std::fs::write(path, text) {
        say(h, &format!("Couldn't save {}: {e}", path.display()), deck_ui::Tone::Error);
    }
}

fn save_public(h: HWND) {
    let Some(key) = current(h) else { return };
    let Some(path) = gui::save_file(Some(h), "Save public key as:", &[("All Files (*.*)", "*.*")], "") else { return };
    match rfc4716(&key) {
        Ok(text) => write(h, &path, &text),
        Err(e) => say(h, &format!("{e:#}"), deck_ui::Tone::Error),
    }
}

fn save_private(h: HWND) {
    let Some(key) = current(h) else { return };
    let Some(pass) = passphrase(h) else { return };
    let Some(path) = gui::save_file(Some(h), "Save private key as:", gui::KEY_FILES, "ppk") else { return };
    let text = ppk::blobs_of(&key).and_then(|(algorithm, public, private)| {
        let argon = (!pass.is_empty()).then(super::new_argon).transpose()?;
        Ppk::write(&algorithm, &key.comment().to_string(), &public, &private, &pass, argon)
    });
    match text {
        Ok(text) => write(h, &path, &text),
        Err(e) => say(h, &format!("{e:#}"), deck_ui::Tone::Error),
    }
}

/// Conversions > Export OpenSSH key: OpenSSH's own format, encrypted with the
/// passphrase if there is one.
fn export_openssh(h: HWND) {
    let Some(key) = current(h) else { return };
    let Some(pass) = passphrase(h) else { return };
    let Some(path) = gui::save_file(Some(h), "Save OpenSSH private key as:", &[("All Files (*.*)", "*.*")], "") else { return };
    let protected = if pass.is_empty() { Ok(key.clone()) } else { key.encrypt(&mut rand::rng(), &pass) };
    match protected.and_then(|k| k.to_openssh(LineEnding::LF).map(|t| t.to_string())) {
        Ok(text) => write(h, &path, &text),
        Err(e) => say(h, &format!("{e}"), deck_ui::Tone::Error),
    }
}

fn load_from_picker(h: HWND, title: &str) {
    if let Some(path) = gui::open_file(Some(h), title, gui::KEY_FILES) {
        load_file(h, &path.to_string_lossy());
    }
}

fn layout(h: HWND) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(h, &mut r);
    }
    let p = |v| gui::px(h, v);
    let (gap, row, label, bw) = (p(12), p(26), p(20), p(150));
    let width = r.right - gap * 2;
    let mut y = gap;
    place(item(h, STATUS), gap, y, width, label);
    y += label + p(4);
    place(item(h, PUBLIC_LABEL), gap, y, width, label);
    y += label;
    place(item(h, PUBLIC), gap, y, width, p(70));
    y += p(70) + p(8);
    // the labelled boxes, with their labels in a column on the left
    for (l, b) in [(PRINT_LABEL, PRINT), (COMMENT_LABEL, COMMENT), (PASS_LABEL, PASS), (AGAIN_LABEL, AGAIN)] {
        place(item(h, l), gap, y + p(4), bw, label);
        place(item(h, b), gap + bw, y, width - bw, row);
        y += row + p(6);
    }
    y += p(8);
    for (l, buttons) in [(GENERATE_LABEL, vec![GENERATE]), (LOAD_LABEL, vec![LOAD]), (SAVE_LABEL, vec![SAVE_PUBLIC, SAVE_PRIVATE])] {
        place(item(h, l), gap, y + p(5), width - p(250), label);
        let mut x = r.right - gap;
        for &b in buttons.iter().rev() {
            x -= p(120);
            place(item(h, b), x, y, p(116), row + p(2));
        }
        y += row + p(8);
    }
    y += p(8);
    place(item(h, TYPE_LABEL), gap, y + p(4), bw, label);
    let mut x = gap + bw;
    for id in [RSA, ECDSA, EDDSA] {
        place(item(h, id), x, y, p(90), row);
        x += p(96);
    }
    y += row + p(6);
    place(item(h, BITS_LABEL), gap, y + p(4), p(240), label);
    place(item(h, BITS), gap + p(250), y, p(80), row);
}

/// The bits box follows the type, as PuTTYgen's does.
fn type_chosen(h: HWND) {
    let (bits, on) = if checked(h, RSA) {
        ("2048", true)
    } else if checked(h, ECDSA) {
        ("256", true)
    } else {
        ("255", false)
    };
    set_text(item(h, BITS), bits);
    unsafe {
        let _ = EnableWindow(item(h, BITS), on);
    }
}

unsafe extern "system" fn proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if let Some(r) = gui::themed(h, msg, wp, lp) {
        return r;
    }
    match msg {
        WM_SIZE => layout(h),
        WM_MADE => made(h),
        WM_COMMAND => match wp.0 & 0xffff {
            GENERATE => generate(h),
            LOAD => load_from_picker(h, "Load private key:"),
            IDM_IMPORT => load_from_picker(h, "Load OpenSSH private key:"),
            SAVE_PUBLIC => save_public(h),
            SAVE_PRIVATE => save_private(h),
            IDM_EXPORT_OPENSSH | IDM_EXPORT_OPENSSH_NEW => export_openssh(h),
            RSA | ECDSA | EDDSA => type_chosen(h),
            IDM_EXIT => {
                let _ = DestroyWindow(h);
            }
            IDM_ABOUT => say(h, concat!("PuTTYgen, from RUSTuTTY ", env!("CARGO_PKG_VERSION"), "\n\nPuTTY's key generator rewritten in Rust.\nAn unofficial program, not connected with the PuTTY project."), deck_ui::Tone::Info),
            IDM_HELP => {
                let _ = ShellExecuteW(None, w!("open"), w!("https://optimusprime-nn.github.io/RUSTuTTY/Tools.html#puttygen"), None, None, SW_SHOWNORMAL);
            }
            _ => {}
        },
        WM_DESTROY => PostQuitMessage(0),
        _ => return DefWindowProcW(h, msg, wp, lp),
    }
    LRESULT(0)
}

/// PuTTYgen's menus, which repeat the buttons and add the conversions.
fn menus() -> HMENU {
    unsafe {
        let bar = CreateMenu().unwrap_or_default();
        let add = |menu: HMENU, items: &[(usize, &str)]| {
            for &(id, text) in items {
                let t = wide(text);
                let _ = if id == 0 { AppendMenuW(menu, MF_SEPARATOR, 0, None) } else { AppendMenuW(menu, MF_STRING, id, PCWSTR(t.as_ptr())) };
            }
        };
        let file = CreatePopupMenu().unwrap_or_default();
        add(file, &[(LOAD, "&Load private key"), (SAVE_PUBLIC, "Save p&ublic key"), (SAVE_PRIVATE, "&Save private key"), (0, ""), (IDM_EXIT, "E&xit")]);
        let key = CreatePopupMenu().unwrap_or_default();
        add(key, &[(GENERATE, "&Generate key pair"), (0, ""), (RSA, "SSH-2 &RSA key"), (ECDSA, "SSH-2 &ECDSA key"), (EDDSA, "SSH-2 EdD&SA key")]);
        let conversions = CreatePopupMenu().unwrap_or_default();
        add(conversions, &[(IDM_IMPORT, "&Import key"), (IDM_EXPORT_OPENSSH, "Export &OpenSSH key"), (IDM_EXPORT_OPENSSH_NEW, "Export OpenSSH key (force &new file format)")]);
        let help = CreatePopupMenu().unwrap_or_default();
        add(help, &[(IDM_ABOUT, "&About"), (IDM_HELP, "&Help")]);
        for (menu, name) in [(file, w!("&File")), (key, w!("&Key")), (conversions, w!("Con&versions")), (help, w!("&Help"))] {
            let _ = AppendMenuW(bar, MF_POPUP, menu.0 as usize, name);
        }
        bar
    }
}

pub fn run(path: Option<String>) {
    unsafe {
        // started from Explorer, the console is this program's own, and goes
        if GetConsoleProcessList(&mut [0u32; 2]) == 1 {
            let _ = ShowWindow(GetConsoleWindow(), SW_HIDE);
            let _ = FreeConsole();
        }
        gui::sharp();
        deck_ui::init(true);
        let instance = gui::instance();
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hIcon: LoadIconW(Some(instance), PCWSTR(1 as *const u16)).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("PuTTYgen"),
            ..Default::default()
        });
        // a test opens it off-screen and in the background, to take a picture
        let test = std::env::var_os("RUSTUTTY_TEST_OFFSCREEN").is_some();
        let (x, ex) = if test { (-4000, WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW) } else { (CW_USEDEFAULT, WINDOW_EX_STYLE(0)) };
        let Ok(h) = CreateWindowExW(ex, w!("PuTTYgen"), w!("PuTTY Key Generator"), WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN, x, CW_USEDEFAULT, 0, 0, None, Some(menus()), Some(instance), None) else { return };
        deck_ui::style_window(h);
        let f = gui::font(h);
        let none = WINDOW_STYLE(0);
        let flat = WINDOW_EX_STYLE(0);
        let edit = WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
        let read_only = WS_TABSTOP | WINDOW_STYLE((ES_AUTOHSCROLL | ES_READONLY) as u32);
        let secret = WS_TABSTOP | WINDOW_STYLE((ES_AUTOHSCROLL | ES_PASSWORD) as u32);
        let button = WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32);
        let radio = WS_TABSTOP | WINDOW_STYLE(BS_AUTORADIOBUTTON as u32);
        control(h, w!("STATIC"), "No key.", none, flat, STATUS, f);
        control(h, w!("STATIC"), "Public key for pasting into OpenSSH authorized_keys file:", none, flat, PUBLIC_LABEL, f);
        control(h, w!("EDIT"), "", WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32), WS_EX_CLIENTEDGE, PUBLIC, f);
        control(h, w!("STATIC"), "Key fingerprint:", none, flat, PRINT_LABEL, f);
        control(h, w!("EDIT"), "", read_only, WS_EX_CLIENTEDGE, PRINT, f);
        control(h, w!("STATIC"), "Key comment:", none, flat, COMMENT_LABEL, f);
        control(h, w!("EDIT"), "", edit, WS_EX_CLIENTEDGE, COMMENT, f);
        control(h, w!("STATIC"), "Key passphrase:", none, flat, PASS_LABEL, f);
        control(h, w!("EDIT"), "", secret, WS_EX_CLIENTEDGE, PASS, f);
        control(h, w!("STATIC"), "Confirm passphrase:", none, flat, AGAIN_LABEL, f);
        control(h, w!("EDIT"), "", secret, WS_EX_CLIENTEDGE, AGAIN, f);
        control(h, w!("STATIC"), "Generate a public/private key pair", none, flat, GENERATE_LABEL, f);
        control(h, w!("BUTTON"), "&Generate", button, flat, GENERATE, f);
        control(h, w!("STATIC"), "Load an existing private key file", none, flat, LOAD_LABEL, f);
        control(h, w!("BUTTON"), "&Load", button, flat, LOAD, f);
        control(h, w!("STATIC"), "Save the generated key", none, flat, SAVE_LABEL, f);
        control(h, w!("BUTTON"), "Save p&ublic key", button, flat, SAVE_PUBLIC, f);
        control(h, w!("BUTTON"), "&Save private key", button, flat, SAVE_PRIVATE, f);
        control(h, w!("STATIC"), "Type of key to generate:", none, flat, TYPE_LABEL, f);
        control(h, w!("BUTTON"), "&RSA", radio | WS_GROUP, flat, RSA, f);
        control(h, w!("BUTTON"), "&ECDSA", radio, flat, ECDSA, f);
        control(h, w!("BUTTON"), "EdD&SA", radio, flat, EDDSA, f);
        control(h, w!("STATIC"), "Number of &bits in a generated key:", none, flat, BITS_LABEL, f);
        control(h, w!("EDIT"), "", edit | WS_GROUP, WS_EX_CLIENTEDGE, BITS, f);
        // EdDSA (Ed25519) first, as PuTTYgen does now
        SendMessageW(item(h, EDDSA), BM_SETCHECK, Some(WPARAM(1)), None);
        type_chosen(h);
        show(h, None, "");
        let _ = SetWindowPos(h, None, 0, 0, gui::px(h, 620), gui::px(h, 600), SWP_NOMOVE | SWP_NOZORDER);
        let _ = ShowWindow(h, if test { SW_SHOWNOACTIVATE } else { SW_SHOW });
        if let Some(p) = path {
            load_file(h, &p);
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if !IsDialogMessageW(h, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let _ = DeleteObject(f.into());
    }
}
