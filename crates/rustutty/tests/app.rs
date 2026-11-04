//! Drives a real RUSTuTTY window. The build under test uses a private clipboard
//! and never takes focus, and everything here is addressed to that process's
//! own windows, so it can run while someone is using the PC.

#[path = "../../deck-core/tests/support/server.rs"]
mod server;

use russh::keys::Algorithm;
use russh::MethodKind;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use windows::core::BOOL;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_TEST_KEY: u32 = WM_APP + 0x10;
const WM_TEST_DUMP: u32 = WM_APP + 0x11;
const WM_TEST_CLIP: u32 = WM_APP + 0x12;
const WM_TEST_SET: u32 = WM_APP + 0x14;
const BM_CLICK: u32 = 0x00F5;
const CTRL: isize = 1;
const SHIFT: isize = 2;

/// A raw TCP peer that records what it receives from each connection.
struct Peer {
    port: u16,
    got: Arc<Mutex<Vec<Arc<Mutex<Vec<u8>>>>>>,
    out: Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
}

impl Peer {
    async fn start() -> Peer {
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        let (got, out): (Arc<Mutex<Vec<_>>>, Arc<Mutex<Vec<_>>>) = Default::default();
        let (g, o) = (got.clone(), out.clone());
        tokio::spawn(async move {
            loop {
                let Ok((s, _)) = l.accept().await else { return };
                let buf = Arc::new(Mutex::new(Vec::new()));
                g.lock().unwrap().push(buf.clone());
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                o.lock().unwrap().push(tx);
                tokio::spawn(serve(s, buf, rx));
            }
        });
        Peer { port, got, out }
    }

    async fn connected(&self, n: usize) {
        let end = Instant::now() + Duration::from_secs(8);
        while self.got.lock().unwrap().len() < n {
            assert!(Instant::now() < end, "only {} of {n} connected", self.got.lock().unwrap().len());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn send(&self, conn: usize, data: &[u8]) {
        self.out.lock().unwrap()[conn].send(data.to_vec()).unwrap();
    }

    fn received(&self, conn: usize) -> Vec<u8> {
        self.got.lock().unwrap().get(conn).map(|b| b.lock().unwrap().clone()).unwrap_or_default()
    }

    async fn wait(&self, conn: usize, want: &[u8]) {
        let end = Instant::now() + Duration::from_secs(8);
        while Instant::now() < end {
            if self.received(conn).windows(want.len()).any(|w| w == want) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("conn {conn} never got {want:?}, has {:?}", self.received(conn));
    }
}

async fn serve(mut s: TcpStream, buf: Arc<Mutex<Vec<u8>>>, mut rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) {
    let mut b = [0u8; 4096];
    loop {
        tokio::select! {
            n = s.read(&mut b) => match n {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.lock().unwrap().extend_from_slice(&b[..n]),
            },
            data = rx.recv() => match data {
                Some(d) => { let _ = s.write_all(&d).await; }
                None => return,
            },
        }
    }
}

struct App {
    child: Child,
    main: HWND,
    content: HWND,
    edit: HWND,
    dir: PathBuf,
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

unsafe extern "system" fn collect(h: HWND, lp: LPARAM) -> BOOL {
    (*(lp.0 as *mut Vec<isize>)).push(h.0 as isize);
    true.into()
}

fn class_of(h: HWND) -> String {
    let mut b = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut b) } as usize;
    String::from_utf16_lossy(&b[..n])
}

fn text_of(h: HWND) -> String {
    let mut b = [0u16; 256];
    let n = unsafe { GetWindowTextW(h, &mut b) } as usize;
    String::from_utf16_lossy(&b[..n])
}

/// The main window of this process only, never anyone else's.
fn window_of(pid: u32) -> Option<HWND> {
    let mut all: Vec<isize> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut all as *mut _ as isize));
    }
    all.into_iter().map(|h| HWND(h as *mut _)).find(|&h| {
        let mut p = 0;
        unsafe { GetWindowThreadProcessId(h, Some(&mut p)) };
        p == pid && class_of(h) == "RUSTuTTYMain"
    })
}

fn children(h: HWND) -> Vec<HWND> {
    let mut all: Vec<isize> = Vec::new();
    unsafe {
        let _ = EnumChildWindows(Some(h), Some(collect), LPARAM(&mut all as *mut _ as isize));
    }
    all.into_iter().map(|h| HWND(h as *mut _)).collect()
}

impl App {
    fn start(args: &[String]) -> App {
        let dir = std::env::temp_dir().join(format!("rustutty-test-{}-{}", std::process::id(), rand_tag()));
        std::fs::create_dir_all(&dir).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_rustutty"))
            .args(args)
            .env("RUSTUTTY_TEST_DUMP", dir.join("dump.txt"))
            .env("RUSTUTTY_TEST_CLIP", dir.join("clip.txt"))
            .env("RUSTUTTY_TEST_SET", dir.join("set.txt"))
            .spawn()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        let main = loop {
            if let Some(h) = window_of(child.id()) {
                break h;
            }
            assert!(Instant::now() < end, "window never appeared");
            std::thread::sleep(Duration::from_millis(20));
        };
        // the window exists before its controls do
        let (content, edit) = loop {
            let kids = children(main);
            let find = |c: &str| kids.iter().find(|&&h| class_of(h) == c).copied();
            if let (Some(c), Some(e), true) = (find("RUSTuTTYContent"), find("Edit"), kids.len() >= 11) {
                break (c, e);
            }
            assert!(Instant::now() < end, "controls never appeared");
            std::thread::sleep(Duration::from_millis(20));
        };
        let app = App { child, main, content, edit, dir };
        let tick = Instant::now() + Duration::from_secs(10);
        while !app.try_dump() {
            assert!(Instant::now() < tick, "deck never became ready");
        }
        app
    }

    fn try_dump(&self) -> bool {
        let path = self.dir.join("dump.txt");
        let _ = std::fs::remove_file(&path);
        if unsafe { PostMessageW(Some(self.main), WM_TEST_DUMP, WPARAM(0), LPARAM(0)) }.is_err() {
            return false;
        }
        for _ in 0..50 {
            if path.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    fn post(&self, h: HWND, msg: u32, wp: usize, lp: isize) {
        unsafe { PostMessageW(Some(h), msg, WPARAM(wp), LPARAM(lp)).unwrap() };
    }

    fn dump(&self) -> String {
        let path = self.dir.join("dump.txt");
        let _ = std::fs::remove_file(&path);
        self.post(self.main, WM_TEST_DUMP, 0, 0);
        let end = Instant::now() + Duration::from_secs(4);
        while Instant::now() < end {
            if let Ok(s) = std::fs::read_to_string(&path) {
                return s;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("no dump");
    }

    async fn until(&self, what: &str, f: impl Fn(&str) -> bool) -> String {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let d = self.dump();
            if f(&d) {
                return d;
            }
            assert!(Instant::now() < end, "waiting for {what}, have:\n{d}");
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }

    async fn sees(&self, text: &str) -> String {
        self.until(text, |d| d.contains(text)).await
    }

    fn typed(&self, text: &str) {
        for u in text.encode_utf16() {
            self.post(self.content, WM_CHAR, u as usize, 0);
        }
    }

    fn key(&self, vk: u8, flags: isize) {
        self.post(self.main, WM_TEST_KEY, vk as usize, flags);
    }

    /// Changes the session's settings, as Change Settings would.
    fn set(&self, settings: &[(&str, &str)]) {
        let text: String = settings.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
        std::fs::write(self.dir.join("set.txt"), text).unwrap();
        self.post(self.main, WM_TEST_SET, 0, 0);
    }

    fn set_clip(&self, text: &str) {
        std::fs::write(self.dir.join("clip.txt"), text).unwrap();
        self.post(self.main, WM_TEST_CLIP, 0, 0);
    }

    fn click(&self, label: &str) {
        let b = children(self.main).into_iter().find(|&h| class_of(h) == "Button" && text_of(h) == label).unwrap();
        self.post(b, BM_CLICK, 0, 0);
    }

    fn open(&self, target: &str) {
        let t: Vec<u16> = target.encode_utf16().chain([0]).collect();
        unsafe { SendMessageW(self.edit, WM_SETTEXT, None, Some(LPARAM(t.as_ptr() as isize))) };
        self.click("Open");
    }

    fn drag(&self, from: (i32, i32), to: (i32, i32)) {
        let cell = self.cell();
        let at = |c: (i32, i32)| ((c.1 * cell.1 + cell.1 / 2) << 16 | (c.0 * cell.0 + cell.0 / 2)) as isize;
        self.post(self.content, WM_LBUTTONDOWN, 1, at(from));
        self.post(self.content, WM_MOUSEMOVE, 1, at(to));
        self.post(self.content, WM_LBUTTONUP, 0, at(to));
    }

    fn cell(&self) -> (i32, i32) {
        let d = self.dump();
        let g = field(&d, "grid=");
        let (c, r) = g.split_once('x').unwrap();
        let mut rect = RECT::default();
        unsafe { GetClientRect(self.content, &mut rect).unwrap() };
        (rect.right / c.parse::<i32>().unwrap(), rect.bottom / r.parse::<i32>().unwrap())
    }

    fn resize(&self, w: i32, h: i32) {
        unsafe { SetWindowPos(self.main, None, -4000, 40, w, h, SWP_NOZORDER | SWP_NOACTIVATE).unwrap() };
    }

    fn exited(&mut self) -> bool {
        let end = Instant::now() + Duration::from_secs(8);
        while Instant::now() < end {
            if self.child.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

fn rand_tag() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn field<'a>(d: &'a str, key: &str) -> &'a str {
    d.lines().find_map(|l| l.strip_prefix(key)).unwrap_or("")
}

fn raw_args(port: u16) -> Vec<String> {
    vec!["-raw".into(), "-P".into(), port.to_string(), "127.0.0.1".into()]
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_switch_and_keep_their_own_screens() {
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    app.until("first session", |d| d.contains("sessions=1")).await;
    app.open(&format!("raw://127.0.0.1:{}", p.port));
    app.until("second session", |d| d.contains("sessions=2") && d.contains("active=Some(1)")).await;
    p.connected(2).await;
    p.send(0, b"screen zero");
    p.send(1, b"screen one");
    app.sees("row=screen one").await;

    app.typed("x");
    p.wait(1, b"x").await;
    assert!(p.received(0).is_empty(), "typing goes only to the selected session");

    app.key(VK_TAB.0 as u8, CTRL);
    let d = app.sees("row=screen zero").await;
    assert!(d.contains("active=Some(0)"));
    app.key(b'2', CTRL | SHIFT);
    app.sees("row=screen one").await;
    app.key(VK_TAB.0 as u8, CTRL | SHIFT);
    app.sees("row=screen zero").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn copy_and_paste_rules() {
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    app.until("session", |d| d.contains("sessions=1")).await;
    p.connected(1).await;

    app.set_clip("hello-paste");
    app.key(b'V', CTRL | SHIFT);
    p.wait(0, b"hello-paste").await;

    p.send(0, b"copy this text");
    app.sees("row=copy this text").await;
    app.drag((5, 0), (8, 0));
    let d = app.until("selection copied", |d| field(d, "clip=") == "\"this\"").await;
    assert!(d.contains("selection=true"));

    app.set_clip("other");
    app.key(b'C', CTRL | SHIFT);
    let d = app.until("copied again", |d| field(d, "clip=") == "\"this\"").await;
    assert!(d.contains("selection=true"), "copying leaves the selection, as in PuTTY");

    app.key(VK_LEFT.0 as u8, 0);
    p.wait(0, b"\x1b[D").await;

    // a click selects nothing, so the clipboard must survive it
    app.drag((3, 0), (3, 0));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(field(&app.dump(), "clip="), "\"this\"");

    app.click("Paste");
    p.wait(0, b"this").await;

    app.click("Copy all");
    app.until("copy all", |d| field(d, "clip=").contains("copy this text")).await;
}

/// As in PuTTY, plain Ctrl+C and Ctrl+V belong to the program: neither
/// copies nor pastes, even with text selected.
#[tokio::test(flavor = "multi_thread")]
async fn plain_ctrl_c_and_v_go_to_the_program() {
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    app.until("session", |d| d.contains("sessions=1")).await;
    p.connected(1).await;
    p.send(0, b"some text");
    app.sees("row=some text").await;
    app.drag((0, 0), (3, 0));
    app.until("selected", |d| field(d, "clip=") == "\"some\"").await;
    app.set_clip("nope");
    app.key(b'C', CTRL);
    app.key(b'V', CTRL);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let d = app.dump();
    assert_eq!(field(&d, "clip="), "\"nope\"", "Ctrl+C didn't copy");
    assert!(d.contains("selection=true"));
    assert!(!String::from_utf8_lossy(&p.received(0)).contains("nope"), "Ctrl+V didn't paste");
}

#[tokio::test(flavor = "multi_thread")]
async fn paste_to_all_reaches_every_session_and_only_once() {
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    app.until("first", |d| d.contains("sessions=1")).await;
    for n in 2..=3 {
        app.open(&format!("raw://127.0.0.1:{}", p.port));
        app.until("session", move |d| d.contains(&format!("sessions={n}"))).await;
    }
    p.connected(3).await;
    app.set_clip("to-everyone");
    for burst in 1..=20 {
        app.click("Paste to all");
        let want = b"to-everyone".repeat(burst);
        for c in 0..3 {
            p.wait(c, &want).await;
        }
    }
    for c in 0..3 {
        assert_eq!(p.received(c).len(), 20 * 11, "conn {c}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn background_bell_and_titles_show_in_the_list() {
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    app.until("first", |d| d.contains("sessions=1")).await;
    app.open(&format!("raw://127.0.0.1:{}", p.port));
    app.until("second", |d| d.contains("sessions=2")).await;
    p.connected(2).await;
    p.send(0, b"\x1b]0;my-box\x07");
    app.sees("label0=my-box").await;
    p.send(0, b"\x07");
    app.sees("label0=* my-box").await;
    app.key(b'1', CTRL | SHIFT);
    app.sees("label0=my-box\n").await;
    assert!(text_of(app.main).contains("my-box"));
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_close_and_the_deck_goes_with_them() {
    let p = Peer::start().await;
    let mut app = App::start(&raw_args(p.port));
    app.until("first", |d| d.contains("sessions=1")).await;
    app.open(&format!("raw://127.0.0.1:{}", p.port));
    app.until("second", |d| d.contains("sessions=2")).await;

    app.key(b'W', CTRL | SHIFT);
    app.until("one left", |d| d.contains("sessions=1") && d.contains("active=Some(0)")).await;
    app.key(b'D', CTRL | SHIFT);
    app.until("duplicated", |d| d.contains("sessions=2")).await;

    app.post(app.main, WM_CLOSE, 0, 0);
    assert!(app.exited(), "deck should exit");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_close_box_closes_its_own_row_only() {
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    app.until("first", |d| d.contains("sessions=1")).await;
    app.open(&format!("raw://127.0.0.1:{}", p.port));
    app.until("second", |d| d.contains("sessions=2") && d.contains("active=Some(1)")).await;

    assert_ne!(unsafe { GetClassLongPtrW(app.main, GCLP_HICON) }, 0, "the title bar needs an icon");
    let list = children(app.main).into_iter().find(|&h| class_of(h) == "ListBox").unwrap();
    let mut r = RECT::default();
    unsafe { GetClientRect(list, &mut r).unwrap() };
    let at = |x: i32| x as isize & 0xffff | 8 << 16;

    // A click on the row selects it
    app.post(list, WM_LBUTTONDOWN, 1, at(20));
    app.post(list, WM_LBUTTONUP, 0, at(20));
    app.until("selected", |d| d.contains("sessions=2") && d.contains("active=Some(0)")).await;

    // Pressing the box and letting go elsewhere closes nothing
    app.post(list, WM_LBUTTONDOWN, 1, at(r.right - 6));
    app.post(list, WM_LBUTTONUP, 0, at(20));
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(app.dump().contains("sessions=2"));

    app.post(list, WM_LBUTTONDOWN, 1, at(r.right - 6));
    app.post(list, WM_LBUTTONUP, 0, at(r.right - 6));
    app.until("closed", |d| d.contains("sessions=1")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ended_session_can_be_restarted_from_the_window_menu() {
    const IDM_RESTART: usize = 0x40;
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    p.connected(1).await;
    // Refused while it's still running
    app.post(app.main, WM_SYSCOMMAND, IDM_RESTART, 0);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(p.got.lock().unwrap().len(), 1, "a live session isn't restarted");

    p.out.lock().unwrap()[0] = tokio::sync::mpsc::unbounded_channel().0;
    app.until("ended", |d| d.contains("(ended)")).await;
    assert!(text_of(app.main).contains("(inactive)"));
    app.post(app.main, WM_SYSCOMMAND, IDM_RESTART, 0);
    p.connected(2).await;
    app.until("running again", |d| d.contains("sessions=1") && !d.contains("(ended)")).await;
    p.send(1, b"back again");
    app.sees("back again").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn special_commands_reach_a_telnet_host() {
    const IDM_SPECIAL_MIN: usize = 0x400;
    let p = Peer::start().await;
    let app = App::start(&[format!("telnet://127.0.0.1:{}", p.port)]);
    p.connected(1).await;
    app.until("open", |d| d.contains("sessions=1")).await;
    app.post(app.main, WM_SYSCOMMAND, IDM_SPECIAL_MIN, 0);
    p.wait(0, &[255, 246]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_connection_leaves_the_session_readable() {
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    app.until("first", |d| d.contains("sessions=1")).await;
    p.connected(1).await;
    p.send(0, b"last words");
    app.sees("row=last words").await;
    p.out.lock().unwrap().clear();
    let d = app.sees("Connection closed by remote host").await;
    assert!(d.contains("row=last words"));
    assert!(d.contains("label0=(ended) 127.0.0.1"));
}

/// The Event Log is a window of its own, with times on its lines, and it
/// keeps up while it's open.
#[tokio::test(flavor = "multi_thread")]
async fn the_event_log_window_keeps_up() {
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    app.until("first", |d| d.contains("sessions=1")).await;
    p.connected(1).await;
    // 0x10 is the window menu's Event Log
    app.post(app.main, WM_SYSCOMMAND, 0x10, 0);
    app.until("the Event Log open", |d| d.contains("eventlog=true")).await;
    let log_text = || {
        let mut all: Vec<isize> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(collect), LPARAM(&mut all as *mut _ as isize));
        }
        let window = all.into_iter().map(|h| HWND(h as *mut _)).find(|&h| {
            let mut pid = 0;
            unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
            pid == app.child.id() && class_of(h) == "RUSTuTTYLog"
        })?;
        let edit = children(window).into_iter().find(|&h| class_of(h) == "Edit")?;
        // another process's edit box has to be asked for its text
        let mut b = vec![0u16; 8192];
        let n = unsafe { SendMessageW(edit, WM_GETTEXT, Some(WPARAM(b.len())), Some(LPARAM(b.as_mut_ptr() as isize))) }.0 as usize;
        Some(String::from_utf16_lossy(&b[..n]))
    };
    let wait_for = |want: String| async move {
        let end = Instant::now() + Duration::from_secs(8);
        loop {
            let text = log_text().unwrap_or_default();
            if text.contains(&want) {
                return text;
            }
            assert!(Instant::now() < end, "{want:?} not in the Event Log: {text:?}");
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    };
    let text = wait_for(format!("Connecting to 127.0.0.1 port {}", p.port)).await;
    let first = text.lines().next().unwrap();
    assert!(first.len() > 20 && first.as_bytes()[4] == b'-' && first.contains('\t'), "a time in front: {first:?}");
    p.out.lock().unwrap().clear();
    wait_for("Connection closed by remote host".into()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_session_from_login_to_exit() {
    let key = server::new_key(Algorithm::Ed25519);
    let (port, seen) = server::start(key, vec![MethodKind::Password]).await;
    let app = App::start(&[]);
    app.open(&format!("{}@127.0.0.1:{port}", server::USER));
    app.sees("Trust this host and connect?").await;
    app.typed("y\r");
    app.sees("password:").await;
    app.typed(&format!("{}\r", server::PASSWORD));
    let d = app.sees("row=welcome").await;
    assert!(!d.contains(server::PASSWORD), "password shown on screen:\n{d}");

    let grid = field(&d, "grid=").to_string();
    let (c, r) = grid.split_once('x').unwrap();
    let pty = seen.lock().unwrap().pty.clone().unwrap();
    assert_eq!((pty.1.to_string().as_str(), pty.2.to_string().as_str()), (c, r), "server told the window size");

    app.typed("ping");
    app.sees("$ ping").await;

    app.resize(900, 500);
    app.until("grid change", |d| field(d, "grid=") != grid).await;
    let end = Instant::now() + Duration::from_secs(5);
    while seen.lock().unwrap().resizes.is_empty() {
        assert!(Instant::now() < end, "server was never told about the resize");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    app.typed("\rexit\r");
    app.sees("exit status 3").await;
    app.sees("(ended)").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn typing_into_a_prompt_is_not_lost_when_keys_come_fast() {
    let key = server::new_key(Algorithm::Ed25519);
    let (port, _) = server::start(key, vec![MethodKind::Password]).await;
    let app = App::start(&[]);
    app.open(&format!("127.0.0.1:{port}"));
    app.sees("Trust this host and connect?").await;
    app.typed("y\r");
    app.sees("login as:").await;
    app.typed("tester\r");
    app.sees("tester@127.0.0.1's password:").await;
}

/// Run by hand: renders the off-screen window to RUSTUTTY_SHOT to look at.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn screenshot() {
    let path = std::env::var("RUSTUTTY_SHOT").unwrap();
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    p.connected(1).await;
    app.open(&format!("raw://127.0.0.1:{}", p.port));
    p.connected(2).await;
    p.send(0, b"\x1b]0;web-01\x07");
    p.send(1, b"\x1b]0;db-02\x07nick@db-02:~$ ls -la\r\n\x1b[1;34mdrwxr-xr-x\x1b[0m  5 nick nick 4096 Oct 30 \x1b[1;32mbin\x1b[0m\r\n\
        -rw-r--r--  1 nick nick  220 Oct 30 .bash_logout\r\n\x1b[7m inverse \x1b[0m \x1b[4munderlined\x1b[0m \x1b[31mred\x1b[33m yellow\x1b[35m magenta\x1b[0m \xe2\x94\x82 box \xc3\xa9\r\n\
        \x1b[38;5;208m256 colour\x1b[0m \x1b[38;2;0;200;255mtrue colour\x1b[0m\r\nnick@db-02:~$ ");
    app.sees("row=nick@db-02:~$").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let script = format!(
        r#"Add-Type -AssemblyName System.Drawing
Add-Type 'using System;using System.Runtime.InteropServices;public class W{{[DllImport("user32.dll")]public static extern bool PrintWindow(IntPtr h,IntPtr d,uint f);[StructLayout(LayoutKind.Sequential)]public struct R{{public int a,b,c,d;}}[DllImport("user32.dll")]public static extern bool GetWindowRect(IntPtr h,out R r);}}'
$h=[IntPtr]{}
$r=New-Object W+R;[void][W]::GetWindowRect($h,[ref]$r)
$b=New-Object Drawing.Bitmap(($r.c-$r.a),($r.d-$r.b));$g=[Drawing.Graphics]::FromImage($b);$d=$g.GetHdc()
[void][W]::PrintWindow($h,$d,2);$g.ReleaseHdc($d);$b.Save('{path}',[Drawing.Imaging.ImageFormat]::Png)"#,
        app.main.0 as isize
    );
    let out = Command::new("powershell").args(["-NoProfile", "-Command", &script]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// Run by hand: what PuTTY can show that a plain terminal can't, to look at.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn screenshot_of_puttys_extras() {
    let path = std::env::var("RUSTUTTY_SHOT").unwrap();
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    p.connected(1).await;
    app.resize(1100, 520);
    app.set(&[("BCE", "1")]);
    p.send(0, "accents: e\u{301} a\u{300} n\u{303}  \x1b[9mstruck through\x1b[0m  \x1b[5mblinks\x1b[0m  \x1b[1mbold\x1b[0m \x1b[4munder\x1b[0m\r\n".as_bytes());
    p.send(0, "Hebrew: \u{5e9}\u{5dc}\u{5d5}\u{5dd} (\u{5e2}\u{5d5}\u{5dc}\u{5dd})   Arabic: \u{645}\u{631}\u{62d}\u{628}\u{627}   \u{644}\u{627}\r\n".as_bytes());
    p.send(0, b"UK set: \x1b(A# costs #5\x1b(B   SCO: \x1b(U\xc3\x89\xc3\x8d\xc2\xbb\x1b(B  \x1b[11m\x01\x02\x03\x04\x05\x06\x1b[10m\r\n");
    p.send(0, b"\x1b]4;1;rgb:ff/88/00\x07\x1b[31mred, changed by the host to orange\x1b[0m   \x1b[44m blue background \x1b[0m\r\n");
    app.sees("row=UK set").await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let script = format!(
        r#"Add-Type -AssemblyName System.Drawing
Add-Type 'using System;using System.Runtime.InteropServices;public class W{{[DllImport("user32.dll")]public static extern bool PrintWindow(IntPtr h,IntPtr d,uint f);[StructLayout(LayoutKind.Sequential)]public struct R{{public int a,b,c,d;}}[DllImport("user32.dll")]public static extern bool GetWindowRect(IntPtr h,out R r);}}'
$h=[IntPtr]{}
$r=New-Object W+R;[void][W]::GetWindowRect($h,[ref]$r)
$b=New-Object Drawing.Bitmap(($r.c-$r.a),($r.d-$r.b));$g=[Drawing.Graphics]::FromImage($b);$d=$g.GetHdc()
[void][W]::PrintWindow($h,$d,2);$g.ReleaseHdc($d);$b.Save('{path}',[Drawing.Imaging.ImageFormat]::Png)"#,
        app.main.0 as isize
    );
    let out = Command::new("powershell").args(["-NoProfile", "-Command", &script]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn test_build_takes_no_focus() {
    let app = App::start(&[]);
    std::thread::sleep(Duration::from_millis(300));
    assert_ne!(unsafe { GetForegroundWindow() }, app.main);
}

/// The settings window opens, every control is built and read back, and what
/// comes out of it connects.
#[tokio::test(flavor = "multi_thread")]
async fn settings_window_round_trips_a_session() {
    let peer = Peer::start().await;
    let app = App::start(&[]);
    app.post(app.main, WM_APP + 0x13, peer.port as usize, 0);
    peer.connected(1).await;
    let dump = app.dump();
    assert!(dump.contains("sessions=1"), "{dump}");
}

/// Keys reach the host in the form a session's settings ask for, through the
/// real window procedure.
#[tokio::test(flavor = "multi_thread")]
async fn the_window_has_a_scrollbar_that_follows_the_scrollback() {
    let peer = Peer::start().await;
    let app = App::start(&[format!("raw://127.0.0.1:{}", peer.port)]);
    peer.connected(1).await;
    peer.send(0, &b"line\r\n".repeat(200));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut info = SCROLLINFO { cbSize: std::mem::size_of::<SCROLLINFO>() as u32, fMask: SIF_ALL, ..Default::default() };
    unsafe { GetScrollInfo(app.content, SB_VERT, &mut info).unwrap() };
    assert!(info.nMax > info.nPage as i32 + 100, "{info:?}");
    assert_eq!(info.nPos, info.nMax - info.nPage as i32 + 1, "the view starts at the bottom");
}

/// Clicks are counted by RUSTuTTY: a double click selects a word, a triple
/// click the line, and a program with the mouse gets every press, however fast.
#[tokio::test(flavor = "multi_thread")]
async fn double_and_triple_clicks() {
    let peer = Peer::start().await;
    let app = App::start(&raw_args(peer.port));
    peer.connected(1).await;
    peer.send(0, b"alpha beta gamma");
    app.sees("row=alpha beta gamma").await;
    let cell = app.cell();
    let at = |col: i32| ((cell.1 / 2) << 16 | (col * cell.0 + cell.0 / 2)) as isize;
    let clicks = |n: usize| {
        for _ in 0..n {
            app.post(app.content, WM_LBUTTONDOWN, 1, at(7));
            app.post(app.content, WM_LBUTTONUP, 0, at(7));
        }
    };
    clicks(2);
    app.until("a word", |d| field(d, "clip=") == "\"beta\"").await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    clicks(3);
    app.until("the line", |d| field(d, "clip=").starts_with("\"alpha beta gamma")).await;
    tokio::time::sleep(Duration::from_millis(800)).await;

    // SGR mouse reporting: both presses of a double click go to the program
    peer.send(0, b"\x1b[?1000h\x1b[?1006h");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = peer.received(0).len();
    clicks(2);
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let got = String::from_utf8_lossy(&peer.received(0)[before..]).to_string();
        if got.matches("\x1b[<0;").count() >= 4 {
            assert_eq!(got.matches('M').count(), 2, "two presses: {got:?}");
            assert_eq!(got.matches('m').count(), 2, "two releases: {got:?}");
            break;
        }
        assert!(Instant::now() < end, "both clicks reported, have {got:?}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

/// LockSize 2, PuTTY's "change the size of the font": when the window gets
/// smaller the session keeps its rows and columns, and the font shrinks.
#[tokio::test(flavor = "multi_thread")]
async fn resizing_can_change_the_font_instead() {
    let peer = Peer::start().await;
    let app = App::start(&raw_args(peer.port));
    peer.connected(1).await;
    app.resize(1100, 700);
    let d = app.until("session", |d| d.contains("term=")).await;
    let size = |d: &str, key: &str| -> (i32, i32) {
        let v = field(d, key);
        let (c, r) = v.split_once('x').unwrap();
        (c.parse().unwrap(), r.parse().unwrap())
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = size(&app.dump(), "term=");
    let _ = d;
    app.set(&[("LockSize", "2")]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    app.resize(800, 500);
    // the window is far smaller, yet the session's rows and columns all still
    // fit in it: the font shrank to make room (in whole sizes, as PuTTY's)
    let d = app.until("the font fitted", |d| {
        let (gc, gr) = size(d, "grid=");
        size(d, "term=") == before && gc >= before.0 && gr >= before.1
    })
    .await;
    assert_eq!(size(&d, "term="), before, "the rows and columns stay");
    // and back to resizing the terminal: it follows the window again
    app.set(&[("LockSize", "0")]);
    app.resize(1200, 760);
    app.until("the terminal follows the window", |d| size(d, "term=") == size(d, "grid=") && size(d, "term=") != before).await;
}

/// xterm's window operations: the host can ask the window's size in pixels
/// and set it in characters.
#[tokio::test(flavor = "multi_thread")]
async fn hosts_can_ask_and_set_the_window_size() {
    let peer = Peer::start().await;
    let app = App::start(&raw_args(peer.port));
    peer.connected(1).await;
    let d = app.until("session", |d| d.contains("sessions=1")).await;
    let (cols, rows) = field(&d, "grid=").split_once('x').map(|(c, r)| (c.parse::<i32>().unwrap(), r.parse::<i32>().unwrap())).unwrap();
    peer.send(0, b"\x1b[14t\x1b[11t");
    peer.wait(0, b"\x1b[1t").await;
    // the text area in pixels: whole cells high and wide
    let got = String::from_utf8_lossy(&peer.received(0)).to_string();
    let size = got.strip_prefix("\x1b[4;").and_then(|r| r.split('t').next()).unwrap_or_else(|| panic!("{got:?}"));
    let (h, w) = size.split_once(';').map(|(h, w)| (h.parse::<i32>().unwrap(), w.parse::<i32>().unwrap())).unwrap();
    assert!(h > 0 && w > 0 && h % rows == 0 && w % cols == 0, "{h}x{w} for {cols}x{rows}");
    peer.send(0, b"\x1b[8;20;60t");
    app.until("resized by the host", |d| field(d, "grid=") == "60x20").await;
}

/// The middle button extends a selection as it goes down and while it's
/// dragged, and Copy leaves the selection in place.
#[tokio::test(flavor = "multi_thread")]
async fn the_extend_button_and_copy() {
    let peer = Peer::start().await;
    let app = App::start(&raw_args(peer.port));
    peer.connected(1).await;
    peer.send(0, b"0123456789abcdef");
    app.sees("row=0123456789abcdef").await;
    app.drag((5, 0), (8, 0));
    app.until("selected", |d| field(d, "clip=") == "\"5678\"").await;
    let cell = app.cell();
    let at = |c: i32| ((cell.1 / 2) << 16 | (c * cell.0 + cell.0 / 2)) as isize;
    app.post(app.content, WM_MBUTTONDOWN, 16, at(3));
    app.post(app.content, WM_MOUSEMOVE, 16, at(1));
    app.post(app.content, WM_MBUTTONUP, 0, at(1));
    app.until("extended from the start", |d| field(d, "clip=") == "\"12345678\"").await;
    app.set_clip("other");
    app.click("Copy");
    let d = app.until("copied", |d| field(d, "clip=") == "\"12345678\"").await;
    assert!(d.contains("selection=true"), "Copy leaves the selection");
}

/// PuTTY's keys with no letter of their own: Ctrl+2 to Ctrl+8, Ctrl+- and
/// Ctrl+Shift+' send control characters, and Pause sends Ctrl+Z.
#[tokio::test(flavor = "multi_thread")]
async fn keys_that_send_control_characters() {
    let peer = Peer::start().await;
    let app = App::start(&raw_args(peer.port));
    peer.connected(1).await;
    for (vk, flags, want) in [(b'2', CTRL, 0x00u8), (b'6', CTRL, 0x1e), (b'8', CTRL, 0x7f), (0xBD, CTRL, 0x1f), (0xDC, CTRL, 0x1c), (0xDE, CTRL | SHIFT, 0x1e), (VK_PAUSE.0 as u8, 0, 0x1a)] {
        let before = peer.received(0).len();
        app.key(vk, flags);
        let end = Instant::now() + Duration::from_secs(5);
        while peer.received(0).len() == before {
            assert!(Instant::now() < end, "key {vk:#x} sent nothing");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(peer.received(0)[before..], [want], "key {vk:#x}");
    }
}

/// In application keypad mode the keypad sends keypad codes with Num Lock
/// off too, and in newline mode Return sends CR LF.
#[tokio::test(flavor = "multi_thread")]
async fn keypad_and_return_follow_the_hosts_modes() {
    let peer = Peer::start().await;
    let app = App::start(&raw_args(peer.port));
    peer.connected(1).await;
    // these test keys come as the keypad's own, not the separate arrow keys
    app.key(VK_UP.0 as u8, 0);
    peer.wait(0, b"\x1b[A").await;
    peer.send(0, b"x\x1b=\x1b[20h");
    app.sees("row=x").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = peer.received(0).len();
    app.key(VK_UP.0 as u8, 0);
    app.key(VK_CLEAR.0 as u8, 0);
    app.key(VK_RETURN.0 as u8, 0);
    let end = Instant::now() + Duration::from_secs(5);
    let want = b"\x1bOx\x1bOu\r\n";
    while peer.received(0).len() < before + want.len() {
        assert!(Instant::now() < end, "have {:?}", String::from_utf8_lossy(&peer.received(0)[before..]));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(&peer.received(0)[before..], want);
}

/// Shift+PgUp scrolls half a screen, Ctrl+PgUp a line, and Ctrl+Shift+PgUp
/// and PgDn go to the top and bottom, as in PuTTY.
#[tokio::test(flavor = "multi_thread")]
async fn page_keys_scroll_as_putty_does() {
    let peer = Peer::start().await;
    let app = App::start(&raw_args(peer.port));
    peer.connected(1).await;
    peer.send(0, &b"line\r\n".repeat(200));
    app.sees("row=line").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let pos = || {
        let mut info = SCROLLINFO { cbSize: std::mem::size_of::<SCROLLINFO>() as u32, fMask: SIF_ALL, ..Default::default() };
        unsafe { GetScrollInfo(app.content, SB_VERT, &mut info).unwrap() };
        (info.nPos, info.nPage as i32)
    };
    let (bottom, page) = pos();
    let after = |vk: VIRTUAL_KEY, flags: isize| {
        app.key(vk.0 as u8, flags);
        std::thread::sleep(Duration::from_millis(150));
        pos().0
    };
    assert_eq!(after(VK_PRIOR, SHIFT), bottom - page / 2, "Shift+PgUp: half a screen");
    assert_eq!(after(VK_PRIOR, CTRL), bottom - page / 2 - 1, "Ctrl+PgUp: one line");
    assert_eq!(after(VK_NEXT, CTRL), bottom - page / 2, "Ctrl+PgDn: one line back");
    assert_eq!(after(VK_PRIOR, CTRL | SHIFT), 0, "Ctrl+Shift+PgUp: the top");
    assert_eq!(after(VK_NEXT, CTRL | SHIFT), bottom, "Ctrl+Shift+PgDn: the bottom");
}

/// Alt and a character sends ESC and the character, by the same path as
/// typing, so a character outside the basic plane arrives whole.
#[tokio::test(flavor = "multi_thread")]
async fn alt_and_a_character() {
    let peer = Peer::start().await;
    let app = App::start(&raw_args(peer.port));
    peer.connected(1).await;
    app.post(app.content, WM_SYSCHAR, 'x' as usize, 0);
    peer.wait(0, b"\x1bx").await;
    app.post(app.content, WM_SYSCHAR, 0xD83D, 0);
    app.post(app.content, WM_SYSCHAR, 0xDE00, 0);
    peer.wait(0, "\x1b\u{1F600}".as_bytes()).await;
}

/// A program that asks for every move (1003) gets moves with no button held,
/// once per cell, and the sideways wheel.
#[tokio::test(flavor = "multi_thread")]
async fn moves_and_the_sideways_wheel_are_reported() {
    let peer = Peer::start().await;
    let app = App::start(&raw_args(peer.port));
    peer.connected(1).await;
    peer.send(0, b"x\x1b[?1003h\x1b[?1006h");
    app.sees("row=x").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let cell = app.cell();
    let at = |c: i32, r: i32| ((r * cell.1 + cell.1 / 2) << 16 | (c * cell.0 + cell.0 / 2)) as isize;
    app.post(app.content, WM_MOUSEMOVE, 0, at(2, 1));
    app.post(app.content, WM_MOUSEMOVE, 0, at(2, 1));
    app.post(app.content, WM_MOUSEMOVE, 0, at(4, 1));
    peer.wait(0, b"\x1b[<35;5;2M").await;
    let got = String::from_utf8_lossy(&peer.received(0)).to_string();
    assert_eq!(got.matches("\x1b[<35;3;2M").count(), 1, "one report per cell: {got:?}");
    let mut r = RECT::default();
    unsafe { GetWindowRect(app.content, &mut r).unwrap() };
    let screen = ((r.top + 20) as isize) << 16 | (r.left + 20) as isize & 0xffff;
    app.post(app.content, WM_MOUSEHWHEEL, 120usize << 16, screen);
    peer.wait(0, b"\x1b[<67;").await;
}

/// Small wheel movements, as a touchpad sends, add up to one step.
#[tokio::test(flavor = "multi_thread")]
async fn the_wheel_adds_up_small_movements() {
    let peer = Peer::start().await;
    let app = App::start(&raw_args(peer.port));
    peer.connected(1).await;
    peer.send(0, b"x\x1b[?1000h\x1b[?1006h");
    app.sees("row=x").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut r = RECT::default();
    unsafe { GetWindowRect(app.content, &mut r).unwrap() };
    let at = ((r.top + 20) as isize) << 16 | (r.left + 20) as isize & 0xffff;
    for _ in 0..3 {
        app.post(app.content, WM_MOUSEWHEEL, 30usize << 16, at);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!String::from_utf8_lossy(&peer.received(0)).contains("\x1b[<64;"), "three quarters of a step sends nothing");
    app.post(app.content, WM_MOUSEWHEEL, 30usize << 16, at);
    peer.wait(0, b"\x1b[<64;").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(String::from_utf8_lossy(&peer.received(0)).matches("\x1b[<64;").count(), 1, "a whole step sends one report");
}

/// Dragging a selection above the text scrolls back, and keeps scrolling while
/// the pointer is held there; dragging below scrolls forward again.
#[tokio::test(flavor = "multi_thread")]
async fn selecting_past_the_edge_scrolls() {
    let peer = Peer::start().await;
    let app = App::start(&raw_args(peer.port));
    peer.connected(1).await;
    let lines: String = (0..200).map(|i| format!("line {i}\r\n")).collect();
    peer.send(0, lines.as_bytes());
    let d = app.sees("row=line 199").await;
    let first_shown: u32 = d.lines().find_map(|l| l.strip_prefix("row=line ")).unwrap().trim().parse().unwrap();
    let pos = || {
        let mut info = SCROLLINFO { cbSize: std::mem::size_of::<SCROLLINFO>() as u32, fMask: SIF_ALL, ..Default::default() };
        unsafe { GetScrollInfo(app.content, SB_VERT, &mut info).unwrap() };
        info.nPos
    };
    let bottom = pos();
    let cell = app.cell();
    let at = |x: i32, y: i32| ((y as i16 as u16 as isize) << 16) | (x as u16 as isize);

    app.post(app.content, WM_LBUTTONDOWN, 1, at(cell.0 * 3, cell.1 * 5));
    app.post(app.content, WM_MOUSEMOVE, 1, at(cell.0 * 3, -cell.1 * 2));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let moved = pos();
    assert!(moved < bottom, "dragging above the text scrolls back: {bottom} -> {moved}");
    // no more mouse moves: holding the pointer there keeps it scrolling
    tokio::time::sleep(Duration::from_millis(300)).await;
    let held = pos();
    assert!(held < moved, "holding above the text keeps scrolling: {moved} -> {held}");

    app.post(app.content, WM_LBUTTONUP, 0, at(cell.0 * 3, -cell.1 * 2));
    let d = app.until("selection copied", |d| field(d, "clip=").contains("line ")).await;
    let clip = field(&d, "clip=");
    assert!(clip.contains(&format!("line {}", first_shown - 1)), "the selection reaches lines that were off screen: {clip}");
    // once it's let go, the position holds
    let held = pos();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(pos(), held, "letting go stops the scrolling");

    // and back down past the bottom
    let rect = {
        let mut r = RECT::default();
        unsafe { GetClientRect(app.content, &mut r).unwrap() };
        r
    };
    app.post(app.content, WM_LBUTTONDOWN, 1, at(cell.0 * 3, cell.1 * 2));
    app.post(app.content, WM_MOUSEMOVE, 1, at(cell.0 * 3, rect.bottom + cell.1 * 2));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(pos() > held, "dragging below the text scrolls forward");
    app.post(app.content, WM_LBUTTONUP, 0, at(cell.0 * 3, rect.bottom + cell.1 * 2));
}

/// The wheel and the scrollbar move back through the scrollback.
#[tokio::test(flavor = "multi_thread")]
async fn the_wheel_and_scrollbar_scroll_back() {
    let peer = Peer::start().await;
    let app = App::start(&[format!("raw://127.0.0.1:{}", peer.port)]);
    peer.connected(1).await;
    peer.send(0, &b"line\r\n".repeat(200));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let pos = || {
        let mut info = SCROLLINFO { cbSize: std::mem::size_of::<SCROLLINFO>() as u32, fMask: SIF_ALL, ..Default::default() };
        unsafe { GetScrollInfo(app.content, SB_VERT, &mut info).unwrap() };
        info.nPos
    };
    let bottom = pos();
    // one notch up, with the pointer over the terminal
    let mut r = RECT::default();
    unsafe { GetWindowRect(app.content, &mut r).unwrap() };
    let at = ((r.top + 20) as isize) << 16 | (r.left + 20) as isize & 0xffff;
    app.post(app.content, WM_MOUSEWHEEL, (120usize) << 16, at);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(pos() < bottom, "the wheel scrolls back: {} -> {}", bottom, pos());
    app.post(app.content, WM_VSCROLL, SB_PAGEUP.0 as usize, 0);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let paged = pos();
    assert!(paged < bottom - 10, "the scrollbar pages back: {bottom} -> {paged}");
    app.post(app.content, WM_MOUSEWHEEL, (-120i16 as u16 as usize) << 16, at);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(pos() > paged, "the wheel scrolls forward");
    // the scrollbar's own menu: Top and Bottom
    app.post(app.content, WM_VSCROLL, SB_TOP.0 as usize, 0);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(pos(), 0, "Top goes to the start of the scrollback");
    app.post(app.content, WM_VSCROLL, SB_BOTTOM.0 as usize, 0);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(pos(), bottom, "Bottom goes back to the end");
}
