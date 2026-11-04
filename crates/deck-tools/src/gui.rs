//! What Pageant's and PuTTYgen's windows share: a passphrase box, the file
//! pickers, and making controls in RUSTuTTY's look.

use std::cell::RefCell;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Drawn at the screen's own scale, as RUSTuTTY's window is. Without it,
/// Windows stretches the window on a scaled screen and the text goes soft.
pub fn sharp() {
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn instance() -> HINSTANCE {
    unsafe { GetModuleHandleW(None).map(|m| m.into()).unwrap_or_default() }
}

/// Pixels at the window's DPI.
pub fn px(h: HWND, v: i32) -> i32 {
    v * unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(h) }.max(96) as i32 / 96
}

/// The UI font at the window's DPI.
pub fn font(h: HWND) -> HFONT {
    deck_ui::ui_font(unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(h) }.max(96), 10, 400)
}

/// A child control in RUSTuTTY's look.
pub fn control(parent: HWND, class: PCWSTR, text: &str, style: WINDOW_STYLE, ex: WINDOW_EX_STYLE, id: usize, font: HFONT) -> HWND {
    unsafe {
        let t = wide(text);
        let h = CreateWindowExW(ex, class, PCWSTR(t.as_ptr()), WS_CHILD | WS_VISIBLE | style, 0, 0, 0, 0, Some(parent), Some(HMENU(id as *mut _)), Some(instance()), None).unwrap_or_default();
        SendMessageW(h, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        deck_ui::style_control(h);
        h
    }
}

pub fn place(h: HWND, x: i32, y: i32, w: i32, height: i32) {
    unsafe {
        let _ = SetWindowPos(h, None, x, y, w.max(0), height.max(0), SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

pub fn text_of(h: HWND) -> String {
    unsafe {
        let mut buf = vec![0u16; GetWindowTextLengthW(h) as usize + 1];
        let n = GetWindowTextW(h, &mut buf) as usize;
        String::from_utf16_lossy(&buf[..n])
    }
}

pub fn set_text(h: HWND, text: &str) {
    unsafe {
        let _ = SetWindowTextW(h, PCWSTR(wide(text).as_ptr()));
    }
}

/// Paints a window's background and its controls' in the theme.
pub unsafe fn themed(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    match msg {
        // moved to a screen at another scale: the size Windows suggests, and
        // the controls' text in a font for that scale
        WM_DPICHANGED => {
            let r = &*(lp.0 as *const RECT);
            let new = font(hwnd);
            unsafe extern "system" fn set(child: HWND, font: LPARAM) -> windows::core::BOOL {
                SendMessageW(child, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
                true.into()
            }
            let _ = EnumChildWindows(Some(hwnd), Some(set), LPARAM(new.0 as isize));
            let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            Some(LRESULT(0))
        }
        WM_ERASEBKGND => {
            let mut r = RECT::default();
            let _ = GetClientRect(hwnd, &mut r);
            deck_ui::fill(HDC(wp.0 as *mut _), &r, deck_ui::palette().bg);
            Some(LRESULT(1))
        }
        _ => deck_ui::dialog_message(hwnd, msg, wp, lp).map(LRESULT),
    }
}

const PROMPT_TEXT: usize = 1;
const PROMPT_AGAIN: usize = 2;

struct Prompt {
    done: bool,
    answer: Option<String>,
    confirm: bool,
}

thread_local! {
    static PROMPT: RefCell<Prompt> = const { RefCell::new(Prompt { done: false, answer: None, confirm: false }) };
}

unsafe extern "system" fn prompt_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if let Some(r) = themed(hwnd, msg, wp, lp) {
        return r;
    }
    match msg {
        WM_COMMAND => match (wp.0 & 0xffff) as i32 {
            id if id == IDOK.0 => {
                let text = GetDlgItem(Some(hwnd), PROMPT_TEXT as i32).map(text_of).unwrap_or_default();
                let again = GetDlgItem(Some(hwnd), PROMPT_AGAIN as i32).map(text_of).unwrap_or_default();
                let confirm = PROMPT.with(|p| p.borrow().confirm);
                if confirm && text != again {
                    deck_ui::tell(Some(hwnd), "Passphrase", "The passphrases don't match.", deck_ui::Tone::Warning);
                    return LRESULT(0);
                }
                PROMPT.with(|p| {
                    let mut p = p.borrow_mut();
                    p.answer = Some(text);
                    p.done = true;
                });
            }
            id if id == IDCANCEL.0 => PROMPT.with(|p| p.borrow_mut().done = true),
            _ => {}
        },
        WM_CLOSE => PROMPT.with(|p| p.borrow_mut().done = true),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

/// Asks for a passphrase in a box of its own, hidden as it's typed. With
/// `confirm`, it's typed twice and must match. `None` is Cancel.
pub fn passphrase(owner: Option<HWND>, title: &str, label: &str, confirm: bool) -> Option<String> {
    unsafe {
        static CLASS: std::sync::Once = std::sync::Once::new();
        CLASS.call_once(|| {
            RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(prompt_proc), hInstance: instance(), hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), lpszClassName: w!("RUSTuTTYPrompt"), ..Default::default() });
        });
        PROMPT.with(|p| *p.borrow_mut() = Prompt { done: false, answer: None, confirm });
        let t = wide(title);
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let h = CreateWindowExW(WS_EX_DLGMODALFRAME, w!("RUSTuTTYPrompt"), PCWSTR(t.as_ptr()), style, 0, 0, 0, 0, owner, None, Some(instance()), None).ok()?;
        deck_ui::style_window(h);
        let f = font(h);
        let p = |v| px(h, v);
        let (w, row, gap) = (p(380), p(26), p(10));
        let label_h = control(h, w!("STATIC"), label, WINDOW_STYLE(0), WINDOW_EX_STYLE(0), 0, f);
        let pass_style = WS_TABSTOP | WINDOW_STYLE((ES_PASSWORD | ES_AUTOHSCROLL) as u32);
        let first = control(h, w!("EDIT"), "", pass_style, WS_EX_CLIENTEDGE, PROMPT_TEXT, f);
        let mut y = gap;
        place(label_h, gap, y, w - gap * 2, p(20));
        y += p(22);
        place(first, gap, y, w - gap * 2, row);
        y += row + gap;
        if confirm {
            let again_label = control(h, w!("STATIC"), "Confirm passphrase:", WINDOW_STYLE(0), WINDOW_EX_STYLE(0), 0, f);
            let again = control(h, w!("EDIT"), "", pass_style, WS_EX_CLIENTEDGE, PROMPT_AGAIN, f);
            place(again_label, gap, y, w - gap * 2, p(20));
            y += p(22);
            place(again, gap, y, w - gap * 2, row);
            y += row + gap;
        }
        let ok = control(h, w!("BUTTON"), "OK", WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32), WINDOW_EX_STYLE(0), IDOK.0 as usize, f);
        let cancel = control(h, w!("BUTTON"), "Cancel", WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32), WINDOW_EX_STYLE(0), IDCANCEL.0 as usize, f);
        place(ok, w - gap * 2 - p(176), y, p(88), row + p(2));
        place(cancel, w - gap - p(88), y, p(88), row + p(2));
        y += row + gap * 2;
        // the outer size for that inner one, centred on the owner or the screen
        let mut r = RECT { left: 0, top: 0, right: w, bottom: y };
        let _ = AdjustWindowRectEx(&mut r, style, false, WS_EX_DLGMODALFRAME);
        let (ow, oh) = (r.right - r.left, r.bottom - r.top);
        let mut around = RECT { left: 0, top: 0, right: GetSystemMetrics(SM_CXSCREEN), bottom: GetSystemMetrics(SM_CYSCREEN) };
        if let Some(o) = owner {
            let _ = GetWindowRect(o, &mut around);
        }
        let _ = SetWindowPos(h, Some(HWND_TOP), around.left + (around.right - around.left - ow) / 2, around.top + (around.bottom - around.top - oh) / 2, ow, oh, SWP_SHOWWINDOW);
        if let Some(o) = owner {
            let _ = EnableWindow(o, false);
        }
        let _ = SetForegroundWindow(h);
        let _ = SetFocus(Some(first));
        let mut msg = MSG::default();
        while !PROMPT.with(|p| p.borrow().done) && GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if !IsDialogMessageW(h, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if let Some(o) = owner {
            let _ = EnableWindow(o, true);
        }
        let _ = DestroyWindow(h);
        PROMPT.with(|p| p.borrow_mut().answer.take())
    }
}

/// The Open or Save As box. `filter` is pairs of a description and a pattern.
fn pick(owner: Option<HWND>, title: &str, filter: &[(&str, &str)], default_ext: &str, save: bool) -> Option<std::path::PathBuf> {
    let mut f: Vec<u16> = Vec::new();
    for (name, pattern) in filter {
        f.extend(name.encode_utf16().chain([0]));
        f.extend(pattern.encode_utf16().chain([0]));
    }
    f.push(0);
    let mut file = vec![0u16; 1024];
    let t = wide(title);
    let ext = wide(default_ext);
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner.unwrap_or_default(),
        lpstrFilter: PCWSTR(f.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrTitle: PCWSTR(t.as_ptr()),
        lpstrDefExt: if default_ext.is_empty() { PCWSTR::null() } else { PCWSTR(ext.as_ptr()) },
        Flags: if save { OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR } else { OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR },
        ..Default::default()
    };
    let chosen = unsafe { if save { GetSaveFileNameW(&mut ofn) } else { GetOpenFileNameW(&mut ofn) } };
    chosen.as_bool().then(|| {
        let n = file.iter().position(|&c| c == 0).unwrap_or(file.len());
        std::path::PathBuf::from(String::from_utf16_lossy(&file[..n]))
    })
}

pub fn open_file(owner: Option<HWND>, title: &str, filter: &[(&str, &str)]) -> Option<std::path::PathBuf> {
    pick(owner, title, filter, "", false)
}

pub fn save_file(owner: Option<HWND>, title: &str, filter: &[(&str, &str)], default_ext: &str) -> Option<std::path::PathBuf> {
    pick(owner, title, filter, default_ext, true)
}

/// PuTTY's key files, then any file.
pub const KEY_FILES: &[(&str, &str)] = &[("PuTTY Private Key Files (*.ppk)", "*.ppk"), ("All Files (*.*)", "*.*")];
