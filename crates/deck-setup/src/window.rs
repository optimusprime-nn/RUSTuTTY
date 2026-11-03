//! The setup window: what will be installed and where, a progress bar while
//! the files are written, and then plainly that RUSTuTTY is installed, with a
//! choice to start it. One window from start to finish, with a taskbar button,
//! so nothing happens out of sight.

use crate::{installed_version, put_files, total_bytes, VERSION};
use std::path::PathBuf;
use std::sync::Mutex;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

const ID_PRIMARY: i32 = IDOK.0;
const ID_CANCEL: i32 = IDCANCEL.0;
const ID_START: i32 = 3;
/// The worker has written more. The amount is in `PROGRESS`.
const WM_PROGRESS: u32 = WM_APP + 1;
/// The worker has finished. Its answer is in `RESULT`.
const WM_FINISHED: u32 = WM_APP + 2;
/// The client area at 96 dpi.
const WIDTH: i32 = 540;
const HEIGHT: i32 = 340;

static PROGRESS: Mutex<(u64, String)> = Mutex::new((0, String::new()));
static RESULT: Mutex<Option<Result<PathBuf, String>>> = Mutex::new(None);

#[derive(PartialEq)]
enum Page {
    Ready,
    Working,
    Done,
    Failed,
}

struct Ui {
    hwnd: HWND,
    body: HWND,
    status: HWND,
    primary: HWND,
    cancel: HWND,
    start: HWND,
    page: Page,
    /// Thousandths written.
    done: u32,
    exe: Option<PathBuf>,
    dpi: u32,
    font: HFONT,
    title: HFONT,
    icon: HICON,
    previous: Option<String>,
}

impl Ui {
    fn px(&self, v: i32) -> i32 {
        v * self.dpi as i32 / 96
    }

    fn bar(&self) -> RECT {
        RECT { left: self.px(28), top: self.px(206), right: self.px(WIDTH - 28), bottom: self.px(214) }
    }

    fn set_text(h: HWND, text: &str) {
        let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
        unsafe {
            let _ = SetWindowTextW(h, PCWSTR(wide.as_ptr()));
        }
    }

    fn show(h: HWND, on: bool) {
        unsafe {
            let _ = ShowWindow(h, if on { SW_SHOW } else { SW_HIDE });
        }
    }

    /// Puts up a page: its words, its buttons, and what's showing.
    fn page(&mut self, page: Page, text: &str) {
        let (primary, cancel) = match page {
            Page::Ready if self.previous.is_some() => ("&Upgrade", Some("Cancel")),
            Page::Ready => ("&Install", Some("Cancel")),
            Page::Working => ("&Install", Some("Cancel")),
            Page::Done => ("&Finish", None),
            Page::Failed => ("&Try again", Some("Close")),
        };
        // The words get the room the progress bar would have when it isn't showing
        let tall = matches!(page, Page::Ready | Page::Failed);
        unsafe {
            let _ = SetWindowPos(self.body, None, 0, 0, self.px(WIDTH - 56), self.px(if tall { 160 } else { 88 }), SWP_NOMOVE | SWP_NOZORDER);
        }
        Ui::set_text(self.body, text);
        Ui::set_text(self.primary, primary);
        if let Some(c) = cancel {
            Ui::set_text(self.cancel, c);
        }
        Ui::show(self.cancel, cancel.is_some());
        Ui::show(self.status, page == Page::Working || page == Page::Done);
        Ui::show(self.start, page == Page::Done);
        let busy = page == Page::Working;
        unsafe {
            let _ = EnableWindow(self.primary, !busy);
            let _ = EnableWindow(self.cancel, !busy);
            if !busy {
                let _ = SetFocus(Some(self.primary));
            }
            let _ = InvalidateRect(Some(self.hwnd), None, true);
        }
        self.page = page;
    }

    fn ready(&mut self) {
        let dir = crate::install_dir().map_or(String::new(), |d| d.display().to_string());
        let text = match &self.previous {
            Some(old) if old == VERSION => format!(
                "RUSTuTTY {VERSION} is already installed. Installing it again replaces its files and keeps your sessions and settings.\n\nFolder: {dir}\n\nClose RUSTuTTY before you continue."
            ),
            Some(old) => format!(
                "This upgrades RUSTuTTY {old} to {VERSION}. Your sessions and settings are kept.\n\nFolder: {dir}\n\nClose RUSTuTTY before you continue."
            ),
            None => format!(
                "RUSTuTTY {VERSION} will be installed for you only. No administrator rights are needed.\n\nFolder: {dir}\n\nYour saved PuTTY sessions and host keys are not changed."
            ),
        };
        self.page(Page::Ready, &text);
    }

    fn begin(&mut self) {
        self.done = 0;
        self.page(Page::Working, &format!("Installing RUSTuTTY {VERSION}..."));
        Ui::set_text(self.status, "Starting");
        *PROGRESS.lock().unwrap() = (0, String::new());
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            let total = total_bytes().max(1);
            let post = |msg| unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(0));
            };
            let result = put_files(&mut |written, name| {
                *PROGRESS.lock().unwrap() = ((written * 1000 / total).min(1000), name.to_string());
                post(WM_PROGRESS);
            });
            *RESULT.lock().unwrap() = Some(result);
            post(WM_FINISHED);
        });
    }

    fn progress(&mut self) {
        let (done, name) = PROGRESS.lock().unwrap().clone();
        self.done = done as u32;
        Ui::set_text(self.status, &if name == "Start menu" { "Adding RUSTuTTY to the Start menu".to_string() } else { format!("Copying {name}") });
        unsafe {
            let bar = self.bar();
            let _ = InvalidateRect(Some(self.hwnd), Some(&bar), false);
        }
    }

    fn finished(&mut self) {
        match RESULT.lock().unwrap().take() {
            Some(Ok(exe)) => {
                self.exe = Some(exe);
                self.done = 1000;
                Ui::set_text(self.status, "Done");
                let how = if cfg!(feature = "test-hooks") { "(A test build: nothing was added to the Start menu.)" } else { "You'll find it in the Start menu. To remove it later, use Settings > Apps > Installed apps." };
                let what = match &self.previous {
                    Some(old) if old != VERSION => format!("RUSTuTTY has been upgraded from {old} to {VERSION}."),
                    _ => format!("RUSTuTTY {VERSION} is installed."),
                };
                self.page(Page::Done, &format!("{what}\n\n{how}"));
            }
            Some(Err(why)) => {
                self.done = 0;
                self.page(Page::Failed, &why);
            }
            None => {}
        }
    }

    fn command(&mut self, id: i32) {
        match (id, &self.page) {
            (ID_PRIMARY, Page::Ready | Page::Failed) => self.begin(),
            (ID_PRIMARY, Page::Done) => {
                let start = unsafe { SendMessageW(self.start, BM_GETCHECK, None, None) }.0 == 1;
                if let (true, Some(exe)) = (start, &self.exe) {
                    let _ = std::process::Command::new(exe).current_dir(exe.parent().unwrap_or(exe)).spawn();
                }
                close(self.hwnd, 0);
            }
            (ID_CANCEL, Page::Ready | Page::Failed) => close(self.hwnd, if self.page == Page::Failed { 1 } else { 2 }),
            // Escape or the close box on the last page just closes
            (ID_CANCEL, Page::Done) => close(self.hwnd, 0),
            _ => {}
        }
    }

    unsafe fn paint(&self, dc: HDC) {
        let p = deck_ui::palette();
        let mut client = RECT::default();
        let _ = GetClientRect(self.hwnd, &mut client);
        deck_ui::fill(dc, &client, p.bg);
        let size = self.px(48);
        let _ = DrawIconEx(dc, self.px(28), self.px(26), self.icon, size, size, 0, None, DI_NORMAL);
        SetBkMode(dc, TRANSPARENT);
        let old = SelectObject(dc, self.title.into());
        SetTextColor(dc, p.text);
        let mut name: Vec<u16> = "RUSTuTTY".encode_utf16().collect();
        let mut r = RECT { left: self.px(90), top: self.px(24), right: client.right - self.px(28), bottom: self.px(56) };
        DrawTextW(dc, &mut name, &mut r, DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(dc, self.font.into());
        SetTextColor(dc, p.muted);
        let mut sub: Vec<u16> = format!("Setup · version {VERSION}").encode_utf16().collect();
        let mut r = RECT { left: self.px(90), top: self.px(54), right: client.right - self.px(28), bottom: self.px(76) };
        DrawTextW(dc, &mut sub, &mut r, DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(dc, old);
        // a line under the header
        let line = RECT { left: 0, top: self.px(92), right: client.right, bottom: self.px(92) + 1 };
        deck_ui::fill(dc, &line, p.border);
        if self.page == Page::Working || self.page == Page::Done {
            let bar = self.bar();
            let radius = (bar.bottom - bar.top) / 2;
            deck_ui::round(dc, bar, radius, Some(p.control), None);
            let width = (bar.right - bar.left) * self.done as i32 / 1000;
            if width > radius * 2 {
                let filled = RECT { right: bar.left + width, ..bar };
                deck_ui::round(dc, filled, radius, Some(if self.page == Page::Done { p.good } else { p.accent }), None);
            }
        }
    }
}

fn close(hwnd: HWND, code: i32) {
    unsafe {
        let _ = DestroyWindow(hwnd);
        PostQuitMessage(code);
    }
}

unsafe fn ui<'a>(hwnd: HWND) -> Option<&'a mut Ui> {
    (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ui).as_mut()
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(u) = ui(hwnd) else { return DefWindowProcW(hwnd, msg, wp, lp) };
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            u.paint(dc);
            let _ = EndPaint(hwnd, &ps);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_NOTIFY => {
            let from = (*(lp.0 as *const windows::Win32::UI::Controls::NMHDR)).hwndFrom;
            if let Some(answer) = deck_ui::custom_draw(lp, from == u.primary) {
                return LRESULT(answer);
            }
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let Some(answer) = deck_ui::ctl_colour(msg, wp, lp) else { return DefWindowProcW(hwnd, msg, wp, lp) };
            if HWND(lp.0 as *mut _) == u.status {
                SetTextColor(HDC(wp.0 as *mut _), deck_ui::palette().muted);
            }
            return LRESULT(answer);
        }
        WM_COMMAND => u.command((wp.0 & 0xffff) as i32),
        WM_PROGRESS => u.progress(),
        WM_FINISHED => u.finished(),
        // Closing mid-way would leave half an install, so it waits
        WM_CLOSE if u.page == Page::Working => {}
        WM_CLOSE => u.command(ID_CANCEL),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

/// Shows the window and runs it. Returns 0 once installed, 2 if cancelled.
pub fn run() -> i32 {
    unsafe {
        let instance = GetModuleHandleW(None).map(|h| HINSTANCE(h.0)).unwrap_or_default();
        let class = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hIcon: LoadIconW(Some(instance), PCWSTR(1 as *const u16)).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("RUSTuTTYSetup"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
        let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("RUSTuTTYSetup"), w!("RUSTuTTY Setup"), style, CW_USEDEFAULT, CW_USEDEFAULT, 100, 100, None, None, Some(instance), None) else {
            return 1;
        };
        let dpi = GetDpiForWindow(hwnd).max(96);
        let px = |v: i32| v * dpi as i32 / 96;
        // centred on the screen it opened on
        let mut outer = RECT { right: px(WIDTH), bottom: px(HEIGHT), ..Default::default() };
        let _ = AdjustWindowRectExForDpi(&mut outer, style, false, WINDOW_EX_STYLE(0), dpi);
        let (cx, cy) = (outer.right - outer.left, outer.bottom - outer.top);
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let work = info.rcWork;
        let _ = SetWindowPos(hwnd, None, (work.left + work.right - cx) / 2, (work.top + work.bottom - cy) / 2, cx, cy, SWP_NOZORDER);

        let font = deck_ui::ui_font(dpi, 10, 400);
        let child = |class: PCWSTR, text: PCWSTR, extra: u32, id: i32, r: [i32; 4]| {
            let h = CreateWindowExW(WINDOW_EX_STYLE(0), class, text, WS_CHILD | WS_VISIBLE | WINDOW_STYLE(extra), px(r[0]), px(r[1]), px(r[2]), px(r[3]), Some(hwnd), Some(HMENU(id as usize as *mut _)), Some(instance), None).unwrap_or_default();
            SendMessageW(h, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            deck_ui::style_control(h);
            h
        };
        // SS_NOPREFIX, so a folder with & in it shows as it is, and
        // SS_EDITCONTROL, so a long folder wraps instead of running off
        let body = child(w!("STATIC"), w!(""), 0x80 | 0x2000, 0, [28, 108, WIDTH - 56, 160]);
        let status = child(w!("STATIC"), w!(""), 0x80 | 0x4000, 0, [28, 220, WIDTH - 56, 22]); // SS_ENDELLIPSIS
        let start = child(w!("BUTTON"), w!("&Start RUSTuTTY now"), BS_AUTOCHECKBOX as u32 | WS_TABSTOP.0, ID_START, [28, 250, 260, 26]);
        SendMessageW(start, BM_SETCHECK, Some(WPARAM(1)), None);
        let (bw, bh, by) = (112, 34, HEIGHT - 56);
        let primary = child(w!("BUTTON"), w!("&Install"), BS_DEFPUSHBUTTON as u32 | WS_TABSTOP.0, ID_PRIMARY, [WIDTH - 28 - bw, by, bw, bh]);
        let cancel = child(w!("BUTTON"), w!("Cancel"), WS_TABSTOP.0, ID_CANCEL, [WIDTH - 36 - bw * 2, by, bw, bh]);

        let size = px(48);
        let icon = LoadImageW(Some(instance), PCWSTR(1 as *const u16), IMAGE_ICON, size, size, LR_DEFAULTCOLOR).map_or(HICON::default(), |h| HICON(h.0));
        let mut u = Box::new(Ui {
            hwnd,
            body,
            status,
            primary,
            cancel,
            start,
            page: Page::Ready,
            done: 0,
            exe: None,
            dpi,
            font,
            title: deck_ui::ui_font(dpi, 18, 600),
            icon,
            previous: installed_version(),
        });
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, &mut *u as *mut Ui as isize);
        deck_ui::style_window(hwnd);
        u.ready();
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);

        let mut msg = MSG::default();
        let mut code = 0;
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            // Tab, Enter and Escape work as in a dialog
            if IsWindow(Some(hwnd)).as_bool() && IsDialogMessageW(hwnd, &msg).as_bool() {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if msg.message == WM_QUIT {
            code = msg.wParam.0 as i32;
        }
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        for f in [u.font, u.title] {
            let _ = DeleteObject(f.into());
        }
        code
    }
}
