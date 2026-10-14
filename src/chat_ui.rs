//! Native Win32 Chatbox & Rule Manager UI (`dayrecap --chat`).
//!
//! Features:
//! 1. Full standard Windows keyboard shortcuts:
//!    - Ctrl + A (Select All in history or input box)
//!    - Ctrl + C (Copy selected text to clipboard)
//!    - Ctrl + V (Paste into input box)
//!    - Ctrl + X (Cut selected text from input box)
//!    - Ctrl + Z (Undo in input box)
//!    - Ctrl + Y (Redo in input box)
//! 2. Unrestricted text selection: Click, drag, and copy any part of the AI response.
//! 3. Dedicated "📋 Copy Latest Response" button for 1-click clipboard copying.
//! 4. "⏹ Stop" button to cancel an in-progress AI query.
//! 5. Responsive layout with strict minimum window bounds (680x500) and smooth Full-Screen Maximize.

use crate::{attachment::Attachment, config, paths, report, rules, senses, util};
use chrono::Local;
use std::{
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicIsize, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
};

static HWND_HISTORY: AtomicIsize = AtomicIsize::new(0);
static HWND_INPUT: AtomicIsize = AtomicIsize::new(0);

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    Graphics::Gdi::{
        CreateFontW, CreateSolidBrush, DeleteObject, InvalidateRect, SetBkColor, SetTextColor,
        HBRUSH, HDC, HFONT,
    },
    System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        LibraryLoader::{GetProcAddress, LoadLibraryW},
        Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
    },
    UI::{
        Input::KeyboardAndMouse::{EnableWindow, GetKeyState, SetFocus, VK_CONTROL, VK_SHIFT},
        Shell::{
            Shell_NotifyIconW, NOTIFYICONDATAW, NIF_INFO, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
            NIM_MODIFY,
        },
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, FlashWindow,
            GetMessageW, GetWindowTextLengthW, GetWindowTextW, KillTimer, MoveWindow,
            PostMessageW, PostQuitMessage, RegisterClassW, SendMessageW, SetForegroundWindow,
            SetTimer, SetWindowTextW, ShowWindow, TranslateMessage, BM_GETCHECK, BM_SETCHECK,
            BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CW_USEDEFAULT, ES_AUTOVSCROLL,
            ES_MULTILINE, ES_READONLY, MSG, SW_RESTORE, SW_SHOW, WM_APP, WM_CLOSE, WM_COMMAND,
            WM_COPY, WM_CUT, WM_DESTROY, WM_GETMINMAXINFO, WM_PASTE, WM_SETFONT, WM_SIZE,
            WM_TIMER, WNDCLASSW, WS_BORDER, WS_CHILD, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
            WS_OVERLAPPEDWINDOW, WS_SYSMENU, WS_TABSTOP, WS_THICKFRAME, WS_VISIBLE, WS_VSCROLL,
        },
    },
};

#[cfg(windows)]
extern "system" {
    fn MessageBeep(utype: u32) -> i32;
}

const BST_CHECKED: usize = 1;
const BST_UNCHECKED: usize = 0;
const EM_SETSEL: u32 = 0x00B1;
const EM_SCROLLCARET: u32 = 0x00B7;
const EM_UNDO: u32 = 0x00C7;
const EM_REPLACESEL: u32 = 0x00C2;
const CF_UNICODETEXT: u32 = 13;

// Antigravity Dark Theme Palette (BGR for Win32 COLORREF)
const COLOR_BG: u32 = 0x001B1818;      // #18181b
const COLOR_PANEL: u32 = 0x00242020;   // #202024
const COLOR_BTN: u32 = 0x002A2727;     // #27272a
const COLOR_TEXT: u32 = 0x00F5F4F4;    // #f4f4f5
#[allow(dead_code)]
const COLOR_MUTED: u32 = 0x00AAA1A1;   // #a1a1aa

static BRUSH_BG: OnceLock<isize> = OnceLock::new();
static BRUSH_PANEL: OnceLock<isize> = OnceLock::new();
static BRUSH_BTN: OnceLock<isize> = OnceLock::new();

#[cfg(windows)]
unsafe fn brush_bg() -> HBRUSH {
    *BRUSH_BG.get_or_init(|| CreateSolidBrush(COLOR_BG) as isize) as HBRUSH
}

#[cfg(windows)]
unsafe fn brush_panel() -> HBRUSH {
    *BRUSH_PANEL.get_or_init(|| CreateSolidBrush(COLOR_PANEL) as isize) as HBRUSH
}

#[cfg(windows)]
unsafe fn brush_btn() -> HBRUSH {
    *BRUSH_BTN.get_or_init(|| CreateSolidBrush(COLOR_BTN) as isize) as HBRUSH
}

const MIN_WINDOW_WIDTH: i32 = 680;
const MIN_WINDOW_HEIGHT: i32 = 500;

const ID_CHK_WORD: usize = 2001;
const ID_CHK_EXCEL: usize = 2002;
const ID_CHK_FULL_DAY: usize = 2003;
const ID_BTN_RUN_OFFICE: usize = 2004;
const ID_BTN_RUN_ALL: usize = 2005;
const ID_BTN_ACCESS_RULES: usize = 2006;
const ID_BTN_FETCH_WORD: usize = 2007;
const ID_BTN_FETCH_EXCEL: usize = 2008;
const ID_BTN_WHAT_TODAY: usize = 2009;
const ID_BTN_STANDUP: usize = 2010;
const ID_BTN_OPEN_REPORTS: usize = 2011;
const ID_BTN_OPEN_RULES: usize = 2012;
const ID_BTN_COPY_REPLY: usize = 2013;
const ID_BTN_OPEN_CONFIG: usize = 2014;
const ID_EDIT_HISTORY: usize = 2015;
const ID_EDIT_INPUT: usize = 2016;
const ID_BTN_SEND: usize = 2017;
const ID_BTN_STOP: usize = 2018;
const ID_BTN_ATTACH: usize = 2019;

const WM_WORKER_DONE: u32 = WM_APP + 50;
const IDT_THINKING_TIMER: usize = 3001;

#[repr(C)]
struct MinMaxInfo {
    pt_reserved: POINT,
    pt_max_size: POINT,
    pt_max_position: POINT,
    pt_min_track_size: POINT,
    pt_max_track_size: POINT,
}

struct UiState {
    hwnd_main: isize,
    hwnd_status: isize,
    hwnd_chk_word: isize,
    hwnd_chk_excel: isize,
    hwnd_chk_full_day: isize,
    hwnd_btn_run_office: isize,
    hwnd_btn_run_all: isize,
    hwnd_btn_reports: isize,
    hwnd_btn_rules_dir: isize,
    hwnd_btn_copy_reply: isize,
    hwnd_btn_config: isize,
    hwnd_btn_attach: isize,
    hwnd_chip_access: isize,
    hwnd_chip_word: isize,
    hwnd_chip_excel: isize,
    hwnd_chip_today: isize,
    hwnd_chip_standup: isize,
    hwnd_history: isize,
    hwnd_input: isize,
    hwnd_send: isize,
    hwnd_stop: isize,
    busy: bool,
    thinking_seconds: u32,
    cancel_flag: Arc<AtomicBool>,
    latest_reply: String,
    transcript: String,
    chat_history: Vec<(bool, String)>,
    pending_reply: Option<(String, bool)>,
    attached_file: Option<Attachment>,
}

static UI: OnceLock<Mutex<UiState>> = OnceLock::new();

struct InputHistory {
    undo_stack: Vec<String>,
    redo_stack: Vec<String>,
    current_text: String,
    suppress_change: bool,
    sent_prompts: Vec<String>,
    history_idx: Option<usize>,
    saved_draft: String,
}

static INPUT_HISTORY: OnceLock<Mutex<InputHistory>> = OnceLock::new();

fn get_input_history() -> &'static Mutex<InputHistory> {
    INPUT_HISTORY.get_or_init(|| {
        Mutex::new(InputHistory {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            current_text: String::new(),
            suppress_change: false,
            sent_prompts: Vec::new(),
            history_idx: None,
            saved_draft: String::new(),
        })
    })
}

#[cfg(windows)]
unsafe fn navigate_prompt_history(hwnd_in: HWND, go_up: bool) -> bool {
    let mut hist = match get_input_history().lock() {
        Ok(h) => h,
        Err(_) => return false,
    };
    if hist.sent_prompts.is_empty() {
        return false;
    }
    let total = hist.sent_prompts.len();
    if go_up {
        let new_idx = match hist.history_idx {
            None => {
                hist.saved_draft = hist.current_text.clone();
                total.saturating_sub(1)
            }
            Some(curr) => curr.saturating_sub(1),
        };
        hist.history_idx = Some(new_idx);
        let prompt = hist.sent_prompts[new_idx].clone();
        hist.current_text = prompt.clone();
        hist.suppress_change = true;
        SetWindowTextW(hwnd_in, wide(&prompt).as_ptr());
        hist.suppress_change = false;
        let len = prompt.encode_utf16().count();
        SendMessageW(hwnd_in, EM_SETSEL, len, len as LPARAM);
        SendMessageW(hwnd_in, EM_SCROLLCARET, 0, 0);
        true
    } else {
        match hist.history_idx {
            None => false,
            Some(curr) => {
                if curr + 1 < total {
                    let new_idx = curr + 1;
                    hist.history_idx = Some(new_idx);
                    let prompt = hist.sent_prompts[new_idx].clone();
                    hist.current_text = prompt.clone();
                    hist.suppress_change = true;
                    SetWindowTextW(hwnd_in, wide(&prompt).as_ptr());
                    hist.suppress_change = false;
                    let len = prompt.encode_utf16().count();
                    SendMessageW(hwnd_in, EM_SETSEL, len, len as LPARAM);
                    SendMessageW(hwnd_in, EM_SCROLLCARET, 0, 0);
                    true
                } else {
                    hist.history_idx = None;
                    let draft = hist.saved_draft.clone();
                    hist.current_text = draft.clone();
                    hist.suppress_change = true;
                    SetWindowTextW(hwnd_in, wide(&draft).as_ptr());
                    hist.suppress_change = false;
                    let len = draft.encode_utf16().count();
                    SendMessageW(hwnd_in, EM_SETSEL, len, len as LPARAM);
                    SendMessageW(hwnd_in, EM_SCROLLCARET, 0, 0);
                    true
                }
            }
        }
    }
}

#[cfg(windows)]
unsafe fn on_input_text_changed(hwnd_in: HWND) {
    let mut hist = match get_input_history().lock() {
        Ok(h) => h,
        Err(_) => return,
    };
    if hist.suppress_change {
        return;
    }
    let len = GetWindowTextLengthW(hwnd_in);
    let mut buf = vec![0u16; (len as usize) + 2];
    let read = GetWindowTextW(hwnd_in, buf.as_mut_ptr(), buf.len() as i32);
    let new_text = String::from_utf16_lossy(&buf[..read as usize]);

    if new_text == hist.current_text {
        return;
    }

    // Push previous state to undo stack (record up to 50 moves)
    let curr = hist.current_text.clone();
    hist.undo_stack.push(curr);
    if hist.undo_stack.len() > 50 {
        hist.undo_stack.remove(0);
    }
    // New typing clears redo stack
    hist.redo_stack.clear();
    hist.current_text = new_text;
}

#[cfg(windows)]
unsafe fn perform_input_undo(hwnd_in: HWND) {
    let mut hist = match get_input_history().lock() {
        Ok(h) => h,
        Err(_) => return,
    };
    if let Some(prev) = hist.undo_stack.pop() {
        let curr = hist.current_text.clone();
        hist.redo_stack.push(curr);
        if hist.redo_stack.len() > 50 {
            hist.redo_stack.remove(0);
        }
        hist.current_text = prev.clone();
        hist.suppress_change = true;
        SetWindowTextW(hwnd_in, wide(&prev).as_ptr());
        hist.suppress_change = false;
        let len = prev.encode_utf16().count();
        SendMessageW(hwnd_in, EM_SETSEL, len, len as LPARAM);
        SendMessageW(hwnd_in, EM_SCROLLCARET, 0, 0);
    }
}

#[cfg(windows)]
unsafe fn perform_input_redo(hwnd_in: HWND) {
    let mut hist = match get_input_history().lock() {
        Ok(h) => h,
        Err(_) => return,
    };
    if let Some(next) = hist.redo_stack.pop() {
        let curr = hist.current_text.clone();
        hist.undo_stack.push(curr);
        if hist.undo_stack.len() > 50 {
            hist.undo_stack.remove(0);
        }
        hist.current_text = next.clone();
        hist.suppress_change = true;
        SetWindowTextW(hwnd_in, wide(&next).as_ptr());
        hist.suppress_change = false;
        let len = next.encode_utf16().count();
        SendMessageW(hwnd_in, EM_SETSEL, len, len as LPARAM);
        SendMessageW(hwnd_in, EM_SCROLLCARET, 0, 0);
    }
}

pub fn run() {
    rules::ensure_defaults();

    #[cfg(windows)]
    unsafe {
        let class_name = wide("DayRecapChatWindowClass");
        let existing = FindWindowW(class_name.as_ptr(), ptr::null());
        if !existing.is_null() {
            util::log("DayRecapChatWindowClass already exists; restoring and bringing to foreground.");
            ShowWindow(existing, SW_RESTORE);
            SetForegroundWindow(existing);
            return;
        }

        run_win32_chat();
    }
}

#[cfg(windows)]
unsafe fn run_win32_chat() {
    let class_name = wide("DayRecapChatWindowClass");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(chat_wnd_proc),
        lpszClassName: class_name.as_ptr(),
        hbrBackground: brush_bg() as _,
        ..std::mem::zeroed()
    };
    RegisterClassW(&wc);

    let today = Local::now().date_naive();
    let summary = report::load_summary(today);
    let senses_count = senses::load_senses(today).len();
    let active_rules = rules::active_rule_names(today);
    let engine = report::engine_label();

    let welcome = format!(
        "============================================================\r\n\
         DAYRECAP — PERSONAL AI REPORTER & SECRETARY\r\n\
         ============================================================\r\n\
         • Active Time Today : {} ({} notes & Git commits captured)\r\n\
         • Active Rules Today: {}\r\n\
         • Schedule Window   : 8:00 AM – 5:00 PM (excl. 12:00–1:00 PM lunch)\r\n\
         • Auto-Report Time  : 4:40 PM (auto-adjusts hours when run earlier!)\r\n\
         • Connected AI      : {}\r\n\r\n\
         KEYBOARD SHORTCUTS & ERGONOMICS:\r\n\
         • Enter        : Send query or command\r\n\
         • Shift + Enter: Insert a new line (multiline chat box)\r\n\
         • Up / Down    : Recall previous sent prompts (history)\r\n\
         • Ctrl + A     : Select All (in response history or input box)\r\n\
         • Ctrl + C     : Copy selected text to clipboard\r\n\
         • Ctrl + V     : Paste into input box\r\n\
         • Ctrl + Z / Y : Undo / Redo\r\n\
         • [📎 Attach File]: Attach Word, Excel, PDF, images, or code\r\n\
         • You can highlight any response above or click [📋 Copy Latest Response]!\r\n\
         ------------------------------------------------------------\r\n",
        util::fmt_dur(summary.active),
        senses_count,
        active_rules.join(", "),
        engine
    );

    let _ = UI.set(Mutex::new(UiState {
        hwnd_main: 0,
        hwnd_status: 0,
        hwnd_chk_word: 0,
        hwnd_chk_excel: 0,
        hwnd_chk_full_day: 0,
        hwnd_btn_run_office: 0,
        hwnd_btn_run_all: 0,
        hwnd_btn_reports: 0,
        hwnd_btn_rules_dir: 0,
        hwnd_btn_copy_reply: 0,
        hwnd_btn_config: 0,
        hwnd_btn_attach: 0,
        hwnd_chip_access: 0,
        hwnd_chip_word: 0,
        hwnd_chip_excel: 0,
        hwnd_chip_today: 0,
        hwnd_chip_standup: 0,
        hwnd_history: 0,
        hwnd_input: 0,
        hwnd_send: 0,
        hwnd_stop: 0,
        busy: false,
        thinking_seconds: 0,
        cancel_flag: Arc::new(AtomicBool::new(false)),
        latest_reply: String::new(),
        transcript: welcome,
        chat_history: Vec::new(),
        pending_reply: None,
        attached_file: None,
    }));

    let win_style = WS_OVERLAPPEDWINDOW
        | WS_VISIBLE
        | WS_MINIMIZEBOX
        | WS_MAXIMIZEBOX
        | WS_THICKFRAME
        | WS_SYSMENU;

    let hwnd = CreateWindowExW(
        0,
        class_name.as_ptr(),
        wide("DayRecap — Personal AI Reporter & Rules Manager").as_ptr(),
        win_style,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        840,
        680,
        ptr::null_mut(),
        ptr::null_mut(),
        ptr::null_mut(),
        ptr::null(),
    );

    if hwnd.is_null() {
        util::log("Failed to create Win32 chat window");
        return;
    }
    util::log(format!("run_win32_chat: window created, hwnd={:?}", hwnd));

    let font_ui: HFONT = CreateFontW(
        -15,
        0,
        0,
        0,
        400,
        0,
        0,
        0,
        1,
        0,
        0,
        5,
        0,
        wide("Segoe UI").as_ptr(),
    );

    let font_bold: HFONT = CreateFontW(
        -15,
        0,
        0,
        0,
        600,
        0,
        0,
        0,
        1,
        0,
        0,
        5,
        0,
        wide("Segoe UI Semibold").as_ptr(),
    );

    util::log("run_win32_chat: calling build_child_controls");
    build_child_controls(hwnd, font_ui, font_bold);
    util::log("run_win32_chat: calling layout_controls");
    layout_controls(hwnd, 820, 640);
    util::log("run_win32_chat: calling enable_dark_mode");
    enable_dark_mode(hwnd);
    util::log("run_win32_chat: calling ShowWindow");
    ShowWindow(hwnd, SW_SHOW);
    util::log("run_win32_chat: entering GetMessageW loop");

    let mut msg: MSG = std::mem::zeroed();
    while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
        // 1. Enter key in input box: Shift+Enter adds newline, Enter alone sends query
        if msg.message == 0x0100 /* WM_KEYDOWN */ && msg.wParam == 0x0D /* VK_RETURN */ {
            let is_input = msg.hwnd == HWND_INPUT.load(Ordering::Relaxed) as HWND;

            if is_input {
                let shift_down = (GetKeyState(VK_SHIFT as i32) as i16) < 0;
                if shift_down {
                    SendMessageW(msg.hwnd, EM_REPLACESEL, 1, wide("\r\n").as_ptr() as LPARAM);
                    continue;
                } else {
                    submit_input_box();
                    continue;
                }
            }

            let is_button = UI
                .get()
                .and_then(|m| m.lock().ok())
                .map(|s| {
                    msg.hwnd == s.hwnd_btn_run_office as HWND
                        || msg.hwnd == s.hwnd_btn_run_all as HWND
                        || msg.hwnd == s.hwnd_btn_copy_reply as HWND
                        || msg.hwnd == s.hwnd_btn_reports as HWND
                        || msg.hwnd == s.hwnd_btn_rules_dir as HWND
                        || msg.hwnd == s.hwnd_btn_config as HWND
                        || msg.hwnd == s.hwnd_btn_attach as HWND
                        || msg.hwnd == s.hwnd_chip_access as HWND
                        || msg.hwnd == s.hwnd_chip_word as HWND
                        || msg.hwnd == s.hwnd_chip_excel as HWND
                        || msg.hwnd == s.hwnd_chip_today as HWND
                        || msg.hwnd == s.hwnd_chip_standup as HWND
                        || msg.hwnd == s.hwnd_stop as HWND
                })
                .unwrap_or(false);
            if !is_button {
                submit_input_box();
                continue;
            }
        }

        // 2. Up / Down Arrow: Navigate sent prompt history
        if msg.message == 0x0100 /* WM_KEYDOWN */ {
            let is_input = msg.hwnd == HWND_INPUT.load(Ordering::Relaxed) as HWND;
            let ctrl_down = (GetKeyState(VK_CONTROL as i32) as i16) < 0;
            if is_input && !ctrl_down {
                if msg.wParam == 0x26 /* VK_UP */ {
                    if navigate_prompt_history(msg.hwnd, true) {
                        continue;
                    }
                } else if msg.wParam == 0x28 /* VK_DOWN */ {
                    if navigate_prompt_history(msg.hwnd, false) {
                        continue;
                    }
                }
            }
        }

        // 2. Global Keyboard Shortcuts: Ctrl+A, Ctrl+C, Ctrl+V, Ctrl+X, Ctrl+Z, Ctrl+Y
        if msg.message == 0x0100 /* WM_KEYDOWN */ {
            let ctrl_down = (GetKeyState(VK_CONTROL as i32) as i16) < 0;
            if ctrl_down {
                let target = msg.hwnd;
                match msg.wParam as u32 {
                    0x41 /* A */ => {
                        SendMessageW(target, EM_SETSEL, 0, -1 as LPARAM);
                        continue;
                    }
                    0x43 /* C */ => {
                        SendMessageW(target, WM_COPY, 0, 0);
                        continue;
                    }
                    0x56 /* V */ => {
                        SendMessageW(target, WM_PASTE, 0, 0);
                        continue;
                    }
                    0x58 /* X */ => {
                        SendMessageW(target, WM_CUT, 0, 0);
                        continue;
                    }
                    0x5A /* Z */ => {
                        let is_input = target == HWND_INPUT.load(Ordering::Relaxed) as HWND;
                        let shift_down = (GetKeyState(VK_SHIFT as i32) as i16) < 0;
                        if is_input {
                            if shift_down {
                                perform_input_redo(target);
                            } else {
                                perform_input_undo(target);
                            }
                        } else {
                            SendMessageW(target, EM_UNDO, 0, 0);
                        }
                        continue;
                    }
                    0x59 /* Y */ => {
                        let is_input = target == HWND_INPUT.load(Ordering::Relaxed) as HWND;
                        if is_input {
                            perform_input_redo(target);
                        } else {
                            SendMessageW(target, EM_UNDO, 0, 0);
                        }
                        continue;
                    }
                    _ => {}
                }
            }
        }

        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    util::log("run_win32_chat: message loop exited");

    if !font_ui.is_null() {
        DeleteObject(font_ui as _);
    }
    if !font_bold.is_null() {
        DeleteObject(font_bold as _);
    }
}

#[cfg(windows)]
unsafe fn enable_dark_mode(hwnd: HWND) {
    // 1. Dark title bar via dwmapi.dll (Windows 11 / Windows 10 1809+)
    let dwm = LoadLibraryW(wide("dwmapi.dll").as_ptr());
    if !dwm.is_null() {
        let proc = GetProcAddress(dwm, b"DwmSetWindowAttribute\0".as_ptr());
        if let Some(f) = proc {
            type DwmSetWindowAttributeFn =
                unsafe extern "system" fn(HWND, u32, *const std::ffi::c_void, u32) -> i32;
            let set_attr: DwmSetWindowAttributeFn = std::mem::transmute(f);
            let dark_val: i32 = 1;
            let _ = set_attr(hwnd, 20, &dark_val as *const _ as _, 4); // Win11 / Win10 20H1+
            let _ = set_attr(hwnd, 19, &dark_val as *const _ as _, 4); // Win10 1809–1909
        }
    }
}

#[cfg(windows)]
unsafe fn build_child_controls(hwnd: HWND, font_ui: HFONT, font_bold: HFONT) {
    let today = Local::now().date_naive();
    let summary = report::load_summary(today);
    let senses_count = senses::load_senses(today).len();
    let active = rules::active_rule_names(today);
    let engine = report::engine_label();

    let status_text = format!(
        "🟢 Active: {}  |  Breaks: {}  |  Notes: {}  |  AI: {}  |  Auto-Trigger: 4:40 PM",
        util::fmt_dur(summary.active),
        util::fmt_dur(summary.idle),
        senses_count,
        engine
    );

    let mk = |cls: &str, txt: &str, style: u32, id: usize, bold: bool| -> isize {
        let h = CreateWindowExW(
            0,
            wide(cls).as_ptr(),
            wide(txt).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            10,
            10,
            hwnd,
            id as _,
            ptr::null_mut(),
            ptr::null(),
        );
        let f = if bold && !font_bold.is_null() {
            font_bold
        } else {
            font_ui
        };
        if !f.is_null() && !h.is_null() {
            SendMessageW(h, WM_SETFONT, f as WPARAM, 1);
        }
        h as isize
    };

    let btn_chk = (BS_AUTOCHECKBOX as u32) | WS_TABSTOP;
    let btn_def = (BS_DEFPUSHBUTTON as u32) | WS_TABSTOP;
    let btn_std = (BS_PUSHBUTTON as u32) | WS_TABSTOP;

    let hwnd_status = mk("STATIC", &status_text, 0, 0, true);
    let hwnd_chk_word = mk(
        "BUTTON",
        "workReportFormatword (.docx)",
        btn_chk,
        ID_CHK_WORD,
        false,
    );
    let hwnd_chk_excel = mk(
        "BUTTON",
        "ExcelReportFormatExcel (.xlsx)",
        btn_chk,
        ID_CHK_EXCEL,
        false,
    );
    let hwnd_chk_full_day = mk(
        "BUTTON",
        "Full Day (8 AM – 5 PM)",
        btn_chk,
        ID_CHK_FULL_DAY,
        false,
    );

    let word_on = active
        .iter()
        .any(|a| a.eq_ignore_ascii_case(rules::RULE_WORD));
    let excel_on = active
        .iter()
        .any(|a| a.eq_ignore_ascii_case(rules::RULE_EXCEL));
    SendMessageW(
        hwnd_chk_word as HWND,
        BM_SETCHECK,
        if word_on { BST_CHECKED } else { BST_UNCHECKED },
        0,
    );
    SendMessageW(
        hwnd_chk_excel as HWND,
        BM_SETCHECK,
        if excel_on { BST_CHECKED } else { BST_UNCHECKED },
        0,
    );
    // Full day mode checked by default
    SendMessageW(hwnd_chk_full_day as HWND, BM_SETCHECK, BST_CHECKED, 0);

    let hwnd_btn_run_office = mk(
        "BUTTON",
        "⚡ Run Word && Excel Now",
        btn_def,
        ID_BTN_RUN_OFFICE,
        true,
    );
    let hwnd_btn_run_all = mk(
        "BUTTON",
        "📝 1-Click All Reports",
        btn_std,
        ID_BTN_RUN_ALL,
        false,
    );

    // Row 3: 5 Quick AI & Rule Chips
    let hwnd_chip_access = mk(
        "BUTTON",
        "📋 Access Rules",
        btn_std,
        ID_BTN_ACCESS_RULES,
        false,
    );
    let hwnd_chip_word = mk(
        "BUTTON",
        "✏ Word Rule",
        btn_std,
        ID_BTN_FETCH_WORD,
        false,
    );
    let hwnd_chip_excel = mk(
        "BUTTON",
        "✏ Excel Rule",
        btn_std,
        ID_BTN_FETCH_EXCEL,
        false,
    );
    let hwnd_chip_today = mk(
        "BUTTON",
        "📊 What did I do today?",
        btn_std,
        ID_BTN_WHAT_TODAY,
        false,
    );
    let hwnd_chip_standup = mk(
        "BUTTON",
        "🗣 Write my standup",
        btn_std,
        ID_BTN_STANDUP,
        false,
    );

    // Row 4: 5 Attach, Copy, Folder, & Settings Buttons
    let hwnd_btn_attach = mk(
        "BUTTON",
        "📎 Attach File",
        btn_std,
        ID_BTN_ATTACH,
        true,
    );
    let hwnd_btn_copy_reply = mk(
        "BUTTON",
        "📋 Copy Response",
        btn_std,
        ID_BTN_COPY_REPLY,
        true,
    );
    let hwnd_btn_reports = mk(
        "BUTTON",
        "📂 Reports Folder",
        btn_std,
        ID_BTN_OPEN_REPORTS,
        false,
    );
    let hwnd_btn_rules_dir = mk(
        "BUTTON",
        "📋 Rules Folder (.txt / .toml)",
        btn_std,
        ID_BTN_OPEN_RULES,
        false,
    );
    let hwnd_btn_config = mk(
        "BUTTON",
        "⚙ Settings (config.toml)",
        btn_std,
        ID_BTN_OPEN_CONFIG,
        false,
    );

    let initial_transcript = UI
        .get()
        .and_then(|m| m.lock().ok().map(|s| s.transcript.clone()))
        .unwrap_or_default();

    let hwnd_history = mk(
        "EDIT",
        &initial_transcript,
        WS_BORDER
            | WS_VSCROLL
            | (ES_MULTILINE as u32)
            | (ES_AUTOVSCROLL as u32)
            | (ES_READONLY as u32),
        ID_EDIT_HISTORY,
        false,
    );

    let hwnd_input = CreateWindowExW(
        0x00000200, /* WS_EX_CLIENTEDGE */
        wide("EDIT").as_ptr(),
        wide("").as_ptr(),
        WS_CHILD
            | WS_VISIBLE
            | WS_BORDER
            | WS_TABSTOP
            | (ES_MULTILINE as u32)
            | (ES_AUTOVSCROLL as u32)
            | WS_VSCROLL,
        0,
        0,
        10,
        10,
        hwnd,
        ID_EDIT_INPUT as _,
        ptr::null_mut(),
        ptr::null(),
    ) as isize;
    if !font_ui.is_null() && hwnd_input != 0 {
        SendMessageW(hwnd_input as HWND, WM_SETFONT, font_ui as WPARAM, 1);
        SendMessageW(
            hwnd_input as HWND,
            0x1501, /* EM_SETCUEBANNER */
            1,
            wide("💬 Ask questions or type rule changes (Shift+Enter for newline, Enter to send)...")
                .as_ptr() as LPARAM,
        );
    }

    let hwnd_send = mk("BUTTON", "Send ↵", btn_def, ID_BTN_SEND, true);
    let hwnd_stop = mk("BUTTON", "⏹ Stop", btn_std, ID_BTN_STOP, false);
    EnableWindow(hwnd_stop as HWND, 0); // Initially disabled until worker is active

    if let Some(lock) = UI.get() {
        if let Ok(mut s) = lock.lock() {
            s.hwnd_main = hwnd as isize;
            s.hwnd_status = hwnd_status;
            s.hwnd_chk_word = hwnd_chk_word;
            s.hwnd_chk_excel = hwnd_chk_excel;
            s.hwnd_chk_full_day = hwnd_chk_full_day;
            s.hwnd_btn_run_office = hwnd_btn_run_office;
            s.hwnd_btn_run_all = hwnd_btn_run_all;
            s.hwnd_btn_reports = hwnd_btn_reports;
            s.hwnd_btn_rules_dir = hwnd_btn_rules_dir;
            s.hwnd_btn_copy_reply = hwnd_btn_copy_reply;
            s.hwnd_btn_config = hwnd_btn_config;
            s.hwnd_btn_attach = hwnd_btn_attach;
            s.hwnd_chip_access = hwnd_chip_access;
            s.hwnd_chip_word = hwnd_chip_word;
            s.hwnd_chip_excel = hwnd_chip_excel;
            s.hwnd_chip_today = hwnd_chip_today;
            s.hwnd_chip_standup = hwnd_chip_standup;
            s.hwnd_history = hwnd_history;
            s.hwnd_input = hwnd_input;
            s.hwnd_send = hwnd_send;
            s.hwnd_stop = hwnd_stop;
        }
    }

    HWND_HISTORY.store(hwnd_history, Ordering::Relaxed);
    HWND_INPUT.store(hwnd_input, Ordering::Relaxed);

    SetFocus(hwnd_input as HWND);
}

#[cfg(windows)]
unsafe fn layout_controls(hwnd: HWND, w: i32, h: i32) {
    let (
        h_status,
        h_word,
        h_excel,
        h_full_day,
        h_run_office,
        h_run_all,
        chips,
        tools,
        h_history,
        h_input,
        h_send,
        h_stop,
    ) = {
        let Some(lock) = UI.get() else { return };
        let Ok(s) = lock.lock() else { return };
        if s.hwnd_history == 0 {
            return;
        }
        (
            s.hwnd_status as HWND,
            s.hwnd_chk_word as HWND,
            s.hwnd_chk_excel as HWND,
            s.hwnd_chk_full_day as HWND,
            s.hwnd_btn_run_office as HWND,
            s.hwnd_btn_run_all as HWND,
            [
                s.hwnd_chip_access as HWND,
                s.hwnd_chip_word as HWND,
                s.hwnd_chip_excel as HWND,
                s.hwnd_chip_today as HWND,
                s.hwnd_chip_standup as HWND,
            ],
            [
                s.hwnd_btn_attach as HWND,
                s.hwnd_btn_copy_reply as HWND,
                s.hwnd_btn_reports as HWND,
                s.hwnd_btn_rules_dir as HWND,
                s.hwnd_btn_config as HWND,
            ],
            s.hwnd_history as HWND,
            s.hwnd_input as HWND,
            s.hwnd_send as HWND,
            s.hwnd_stop as HWND,
        )
    };

    let pad = 12;
    let gap = 8;
    let usable_w = (w - pad * 2).max(MIN_WINDOW_WIDTH - 40);

    // Row 1: Status Header
    MoveWindow(h_status, pad, 10, usable_w, 22, 1);

    // Row 2: Active Rule Checkboxes + Full Day Checkbox + Primary Action Buttons
    let y2 = 38;
    let chk_w1 = 175.min(usable_w / 5);
    let chk_w2 = 175.min(usable_w / 5);
    let chk_w3 = 160.min(usable_w / 5);
    let total_chk_w = chk_w1 + chk_w2 + chk_w3 + gap * 2;
    let x_btns = pad + total_chk_w + gap;
    let rem_w = (usable_w - total_chk_w - gap).max(180);
    let btn1_w = (rem_w - gap) * 55 / 100;
    let btn2_w = rem_w - gap - btn1_w;

    MoveWindow(h_word, pad, y2, chk_w1, 30, 1);
    MoveWindow(h_excel, pad + chk_w1 + gap, y2, chk_w2, 30, 1);
    MoveWindow(
        h_full_day,
        pad + chk_w1 + chk_w2 + gap * 2,
        y2,
        chk_w3,
        30,
        1,
    );
    MoveWindow(h_run_office, x_btns, y2, btn1_w, 30, 1);
    MoveWindow(
        h_run_all,
        x_btns + btn1_w + gap,
        y2,
        btn2_w,
        30,
        1,
    );

    // Row 3: 5 Quick Action Chips (evenly distributed across full width)
    let y3 = 74;
    let chip_count = chips.len() as i32;
    let chip_w = (usable_w - gap * (chip_count - 1)) / chip_count;
    for (i, &ch) in chips.iter().enumerate() {
        let i_i32 = i as i32;
        let x = pad + i_i32 * (chip_w + gap);
        let cw = if i_i32 == chip_count - 1 {
            usable_w - i_i32 * (chip_w + gap)
        } else {
            chip_w
        };
        MoveWindow(ch, x, y3, cw, 28, 1);
    }

    // Row 4: 4 Folder & Copy Buttons (evenly distributed across full width)
    let y4 = 108;
    let tool_count = tools.len() as i32;
    let tool_w = (usable_w - gap * (tool_count - 1)) / tool_count;
    for (i, &tl) in tools.iter().enumerate() {
        let i_i32 = i as i32;
        let x = pad + i_i32 * (tool_w + gap);
        let tw = if i_i32 == tool_count - 1 {
            usable_w - i_i32 * (tool_w + gap)
        } else {
            tool_w
        };
        MoveWindow(tl, x, y4, tw, 27, 1);
    }

    // Row 5: Main Conversation History Area (stretches vertically & horizontally)
    let y5 = 143;
    let input_h = 68; // Comfortable modern chat input area (3-4 lines with scroll)
    let gap_bottom = 8;
    let bottom_h = input_h + gap_bottom;
    let hist_h = (h - y5 - bottom_h - 12).max(140);
    MoveWindow(h_history, pad, y5, usable_w, hist_h, 1);

    // Row 6: Bottom Input Box + Send Button (top) + Stop Button (bottom)
    let y6 = y5 + hist_h + gap_bottom;
    let action_col_w = 100;
    let input_w = (usable_w - action_col_w - gap).max(200);

    MoveWindow(h_input, pad, y6, input_w, input_h, 1);
    MoveWindow(h_send, pad + input_w + gap, y6, action_col_w, 36, 1);
    MoveWindow(
        h_stop,
        pad + input_w + gap,
        y6 + 38,
        action_col_w,
        28,
        1,
    );

    InvalidateRect(hwnd, ptr::null(), 1);
}

#[cfg(windows)]
unsafe extern "system" fn chat_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_GETMINMAXINFO => {
            if lparam != 0 {
                let mmi = lparam as *mut MinMaxInfo;
                (*mmi).pt_min_track_size.x = MIN_WINDOW_WIDTH;
                (*mmi).pt_min_track_size.y = MIN_WINDOW_HEIGHT as i32;
            }
            0
        }
        WM_SIZE => {
            let w = (lparam & 0xFFFF) as i32;
            let h = ((lparam >> 16) & 0xFFFF) as i32;
            if w > 0 && h > 0 {
                layout_controls(hwnd, w, h);
            }
            0
        }
        0x0133 /* WM_CTLCOLOREDIT */ => {
            let hdc = wparam as HDC;
            SetTextColor(hdc, COLOR_TEXT);
            SetBkColor(hdc, COLOR_PANEL);
            brush_panel() as LRESULT
        }
        0x0138 /* WM_CTLCOLORSTATIC */ => {
            let hdc = wparam as HDC;
            let hist = HWND_HISTORY.load(Ordering::Relaxed);
            if lparam == hist as LPARAM {
                SetTextColor(hdc, COLOR_TEXT);
                SetBkColor(hdc, COLOR_PANEL);
                brush_panel() as LRESULT
            } else {
                SetTextColor(hdc, COLOR_TEXT);
                SetBkColor(hdc, COLOR_BG);
                brush_bg() as LRESULT
            }
        }
        0x0135 /* WM_CTLCOLORBTN */ => {
            let hdc = wparam as HDC;
            SetTextColor(hdc, COLOR_TEXT);
            SetBkColor(hdc, COLOR_BTN);
            brush_btn() as LRESULT
        }
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as usize;
            let code = ((wparam >> 16) & 0xFFFF) as u32;
            if id == ID_EDIT_INPUT && code == 0x0300 /* EN_CHANGE */ {
                on_input_text_changed(lparam as HWND);
            } else if code == 0 /* BN_CLICKED */ || code == 1 {
                handle_command(id);
            }
            0
        }
        WM_TIMER => {
            if wparam as usize == IDT_THINKING_TIMER {
                on_thinking_tick(hwnd);
            }
            0
        }
        WM_WORKER_DONE => {
            on_worker_done();
            0
        }
        WM_CLOSE => {
            util::log("chat_wnd_proc: WM_CLOSE received, destroying window");
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            util::log("chat_wnd_proc: WM_DESTROY received, posting quit");
            KillTimer(hwnd, IDT_THINKING_TIMER);
            clear_tray_notification(hwnd);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(windows)]
unsafe fn handle_command(id: usize) {
    let today = Local::now().date_naive();
    match id {
        ID_CHK_WORD | ID_CHK_EXCEL => {
            let Some(lock) = UI.get() else { return };
            let (chk_word, chk_excel) = match lock.lock() {
                Ok(s) => (s.hwnd_chk_word as HWND, s.hwnd_chk_excel as HWND),
                Err(_) => return,
            };
            let w_on = SendMessageW(chk_word, BM_GETCHECK, 0, 0) as usize == BST_CHECKED;
            let e_on = SendMessageW(chk_excel, BM_GETCHECK, 0, 0) as usize == BST_CHECKED;
            let mut names = Vec::new();
            if w_on {
                names.push(rules::RULE_WORD.to_string());
            }
            if e_on {
                names.push(rules::RULE_EXCEL.to_string());
            }
            if names.is_empty() {
                names.push(rules::RULE_WORD.to_string());
                SendMessageW(chk_word, BM_SETCHECK, BST_CHECKED, 0);
            }
            rules::set_active_rules(today, &names);
            append_message(
                "DayRecap",
                &format!("Active rules for today updated to: {}", names.join(", ")),
            );
        }
        ID_CHK_FULL_DAY => {
            let Some(lock) = UI.get() else { return };
            let is_full = match lock.lock() {
                Ok(s) => {
                    SendMessageW(s.hwnd_chk_full_day as HWND, BM_GETCHECK, 0, 0) as usize
                        == BST_CHECKED
                }
                Err(_) => return,
            };
            append_message(
                "DayRecap",
                if is_full {
                    "Full Day Mode enabled: Reports will cover all 8 hours (8:00 AM – 5:00 PM)."
                } else {
                    "Partial Mode enabled: Reports will auto-adjust up to current morning hour."
                },
            );
        }
        ID_BTN_ATTACH => on_attach_button_clicked(),
        ID_BTN_RUN_OFFICE => trigger_rule_reports(),
        ID_BTN_RUN_ALL => trigger_all_reports(),
        ID_BTN_OPEN_REPORTS => util::open_path(&paths::reports_dir()),
        ID_BTN_OPEN_RULES => util::open_path(&paths::rules_dir()),
        ID_BTN_COPY_REPLY => copy_latest_reply(),
        ID_BTN_OPEN_CONFIG => util::open_path(&paths::config_file()),
        ID_BTN_ACCESS_RULES => send_chat_query("access rules".into()),
        ID_BTN_FETCH_WORD => send_chat_query("fetch workReportFormatword".into()),
        ID_BTN_FETCH_EXCEL => send_chat_query("fetch ExcelReportFormatExcel".into()),
        ID_BTN_WHAT_TODAY => send_chat_query("What did I do today?".into()),
        ID_BTN_STANDUP => send_chat_query("Write my daily standup".into()),
        ID_BTN_SEND => submit_input_box(),
        ID_BTN_STOP => request_stop_worker(),
        _ => {}
    }
}

#[repr(C)]
struct OpenFileNameW {
    l_struct_size: u32,
    hwnd_owner: HWND,
    h_instance: isize,
    lpstr_filter: *const u16,
    lpstr_custom_filter: *mut u16,
    n_max_cust_filter: u32,
    n_filter_index: u32,
    lpstr_file: *mut u16,
    n_max_file: u32,
    lpstr_file_title: *mut u16,
    n_max_file_title: u32,
    lpstr_initial_dir: *const u16,
    lpstr_title: *const u16,
    flags: u32,
    n_file_offset: u16,
    n_file_extension: u16,
    lpstr_def_ext: *const u16,
    l_cust_data: LPARAM,
    lpfn_hook: usize,
    lp_template_name: *const u16,
    pv_reserved: usize,
    dw_reserved: u32,
    flags_ex: u32,
}

#[cfg(windows)]
unsafe fn pick_file_dialog(hwnd_parent: HWND) -> Option<std::path::PathBuf> {
    let comdlg = LoadLibraryW(wide("comdlg32.dll").as_ptr());
    if comdlg.is_null() {
        return None;
    }
    let proc = GetProcAddress(comdlg, b"GetOpenFileNameW\0".as_ptr());
    let get_open_file_name: unsafe extern "system" fn(*mut OpenFileNameW) -> i32 =
        std::mem::transmute(proc?);

    let mut buf = vec![0u16; 1024];
    let mut filter_u16: Vec<u16> = Vec::new();
    let desc = "Supported Files (*.docx;*.xlsx;*.pdf;*.png;*.jpg;*.txt;*.cs;*.csv;*.json)\0*.docx;*.xlsx;*.pdf;*.png;*.jpg;*.jpeg;*.txt;*.cs;*.csv;*.json\0All Files (*.*)\0*.*\0\0";
    for part in desc.split('\0') {
        filter_u16.extend(part.encode_utf16());
        filter_u16.push(0);
    }
    filter_u16.push(0);

    let title = wide("Attach File for DayRecap AI Analysis");

    let mut ofn: OpenFileNameW = std::mem::zeroed();
    ofn.l_struct_size = std::mem::size_of::<OpenFileNameW>() as u32;
    ofn.hwnd_owner = hwnd_parent;
    ofn.lpstr_filter = filter_u16.as_ptr();
    ofn.lpstr_file = buf.as_mut_ptr();
    ofn.n_max_file = buf.len() as u32;
    ofn.lpstr_title = title.as_ptr();
    ofn.flags = 0x00001000 /* OFN_FILEMUSTEXIST */ | 0x00000800 /* OFN_PATHMUSTEXIST */;

    if get_open_file_name(&mut ofn) != 0 {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let path_str = String::from_utf16_lossy(&buf[..end]);
        let trimmed = path_str.trim();
        if !trimmed.is_empty() {
            return Some(std::path::PathBuf::from(trimmed));
        }
    }
    None
}

#[cfg(windows)]
unsafe fn on_attach_button_clicked() {
    let Some(lock) = UI.get() else { return };
    let hwnd_main = match lock.lock() {
        Ok(s) => s.hwnd_main as HWND,
        Err(_) => return,
    };
    if let Some(path) = pick_file_dialog(hwnd_main) {
        match Attachment::from_path(&path) {
            Ok(att) => {
                let msg = format!(
                    "📎 Attached: `{}`\n• Summary: {}\n• Type a message or question, or press Enter / click Send to analyze this file!",
                    att.filename, att.summary
                );
                if let Ok(mut s) = lock.lock() {
                    s.attached_file = Some(att);
                }
                append_message("DayRecap", &msg);
            }
            Err(e) => {
                append_message("DayRecap", &format!("Could not attach file: {e}"));
            }
        }
    }
}

#[cfg(windows)]
unsafe fn copy_latest_reply() {
    let Some(lock) = UI.get() else { return };
    let reply = match lock.lock() {
        Ok(s) => s.latest_reply.clone(),
        Err(_) => return,
    };
    if reply.trim().is_empty() {
        append_message("DayRecap", "No response to copy yet.");
        return;
    }
    copy_to_clipboard(&reply);
    append_message("DayRecap", "📋 Copied the latest response to your clipboard!");
}

#[cfg(windows)]
unsafe fn copy_to_clipboard(text: &str) {
    let wide_text = wide(text);
    let bytes = wide_text.len() * 2;
    if OpenClipboard(ptr::null_mut()) != 0 {
        EmptyClipboard();
        let h_mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if !h_mem.is_null() {
            let ptr = GlobalLock(h_mem) as *mut u16;
            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(wide_text.as_ptr(), ptr, wide_text.len());
                GlobalUnlock(h_mem);
                SetClipboardData(CF_UNICODETEXT, h_mem);
            }
        }
        CloseClipboard();
    }
}

#[cfg(windows)]
unsafe fn request_stop_worker() {
    let Some(lock) = UI.get() else { return };
    if let Ok(mut s) = lock.lock() {
        if s.busy {
            s.cancel_flag.store(true, Ordering::SeqCst);
            s.busy = false;
            KillTimer(s.hwnd_main as HWND, IDT_THINKING_TIMER);
            EnableWindow(s.hwnd_stop as HWND, 0);
            let engine = report::engine_label();
            let status = format!("⏹ Stopped  |  AI: {engine}");
            SetWindowTextW(s.hwnd_status as HWND, wide(&status).as_ptr());
        }
    }
    append_message("DayRecap", "⏹ Canceled request.");
}

#[cfg(windows)]
unsafe fn submit_input_box() {
    let Some(lock) = UI.get() else { return };
    let (hwnd_in, att_opt) = match lock.lock() {
        Ok(mut s) => (s.hwnd_input as HWND, s.attached_file.take()),
        Err(_) => return,
    };
    let len = GetWindowTextLengthW(hwnd_in);
    let mut buf = vec![0u16; (len.max(0) as usize) + 2];
    let read = if len > 0 {
        GetWindowTextW(hwnd_in, buf.as_mut_ptr(), buf.len() as i32)
    } else {
        0
    };
    let text = String::from_utf16_lossy(&buf[..read as usize])
        .trim()
        .to_string();

    if text.is_empty() && att_opt.is_none() {
        return;
    }

    let final_user_prompt = if text.is_empty() {
        if let Some(ref att) = att_opt {
            format!("Analyze and inspect the attached {}", att.filename)
        } else {
            String::new()
        }
    } else {
        text.clone()
    };

    if !text.is_empty() {
        if let Ok(mut hist) = get_input_history().lock() {
            hist.undo_stack.push(text.clone());
            if hist.undo_stack.len() > 50 {
                hist.undo_stack.remove(0);
            }
            hist.sent_prompts.push(text.clone());
            if hist.sent_prompts.len() > 50 {
                hist.sent_prompts.remove(0);
            }
            hist.history_idx = None;
            hist.saved_draft.clear();
            hist.current_text = String::new();
            hist.redo_stack.clear();
            hist.suppress_change = true;
        }
        SetWindowTextW(hwnd_in, wide("").as_ptr());
        if let Ok(mut hist) = get_input_history().lock() {
            hist.suppress_change = false;
        }
    }
    SetFocus(hwnd_in);

    if let Some(att) = att_opt {
        send_chat_query_with_attachment(final_user_prompt, att);
    } else {
        send_chat_query(final_user_prompt);
    }
}

#[cfg(windows)]
unsafe fn send_chat_query(q: String) {
    let Some(lock) = UI.get() else { return };
    let (hwnd_main, hwnd_stop, cancel_flag, history) = {
        let Ok(mut s) = lock.lock() else { return };
        if s.busy {
            return;
        }
        s.busy = true;
        s.cancel_flag.store(false, Ordering::SeqCst);
        EnableWindow(s.hwnd_stop as HWND, 1);
        start_thinking_timer(s.hwnd_main as HWND);
        s.chat_history.push((true, q.clone()));
        (
            s.hwnd_main as HWND,
            s.hwnd_stop as HWND,
            s.cancel_flag.clone(),
            s.chat_history.clone(),
        )
    };

    append_message("You", &q);
    let today = Local::now().date_naive();
    report::save_chat(today, "You", &q);

    let main_id = hwnd_main as isize;
    thread::spawn(move || {
        let answer = if cancel_flag.load(Ordering::SeqCst) {
            "Request was stopped.".into()
        } else {
            report::chat_answer(today, &history, &q)
        };
        if !cancel_flag.load(Ordering::SeqCst) {
            report::save_chat(today, "DayRecap", &answer);
        }
        if let Some(lock) = UI.get() {
            if let Ok(mut s) = lock.lock() {
                s.pending_reply = Some((answer, false));
            }
        }
        PostMessageW(main_id as HWND, WM_WORKER_DONE, 0, 0);
        let _ = hwnd_stop;
    });
}

#[cfg(windows)]
unsafe fn send_chat_query_with_attachment(q: String, att: Attachment) {
    let Some(lock) = UI.get() else { return };
    let (hwnd_main, hwnd_stop, cancel_flag, history) = {
        let Ok(mut s) = lock.lock() else { return };
        if s.busy {
            return;
        }
        s.busy = true;
        s.cancel_flag.store(false, Ordering::SeqCst);
        EnableWindow(s.hwnd_stop as HWND, 1);
        start_thinking_timer(s.hwnd_main as HWND);
        let display_q = format!("📎 [Attached: {}]\r\n{}", att.filename, q);
        s.chat_history.push((true, display_q));
        (
            s.hwnd_main as HWND,
            s.hwnd_stop as HWND,
            s.cancel_flag.clone(),
            s.chat_history.clone(),
        )
    };

    append_message("You", &format!("📎 [Attached: {}]\r\n{}", att.filename, q));
    let today = Local::now().date_naive();
    report::save_chat(today, "You", &format!("[Attached: {}] {}", att.filename, q));

    let full_llm_query = format!("{}\n\n{}", q, att.to_prompt_context());
    let main_id = hwnd_main as isize;
    thread::spawn(move || {
        let answer = if cancel_flag.load(Ordering::SeqCst) {
            "Request was stopped.".into()
        } else {
            report::chat_answer(today, &history, &full_llm_query)
        };
        if !cancel_flag.load(Ordering::SeqCst) {
            report::save_chat(today, "DayRecap", &answer);
        }
        if let Some(lock) = UI.get() {
            if let Ok(mut s) = lock.lock() {
                s.pending_reply = Some((answer, false));
            }
        }
        PostMessageW(main_id as HWND, WM_WORKER_DONE, 0, 0);
        let _ = hwnd_stop;
    });
}

#[cfg(windows)]
unsafe fn trigger_rule_reports() {
    let Some(lock) = UI.get() else { return };
    let (hwnd_main, hwnd_stop, force_full) = {
        let Ok(mut s) = lock.lock() else { return };
        if s.busy {
            return;
        }
        let full = SendMessageW(s.hwnd_chk_full_day as HWND, BM_GETCHECK, 0, 0) as usize
            == BST_CHECKED;
        s.busy = true;
        EnableWindow(s.hwnd_stop as HWND, 1);
        start_thinking_timer(s.hwnd_main as HWND);
        (s.hwnd_main, s.hwnd_stop, full)
    };

    let today = Local::now().date_naive();
    let active = rules::active_rule_names(today);
    let mode_str = if force_full {
        "Full Day 8 AM – 5 PM"
    } else {
        "Auto-Adjusted"
    };
    append_message(
        "You",
        &format!("⚡ [Run Active Word/Excel Rules: {} ({mode_str})]", active.join(", ")),
    );

    thread::spawn(move || {
        let reply = match report::generate_rule_reports_with_mode(today, force_full) {
            Ok(files) => {
                let list: Vec<String> =
                    files.iter().map(|p| format!("• {}", p.display())).collect();
                (
                    format!(
                        "✅ Generated Word (.docx) / Excel (.xlsx) Rule Reports ({}):\n{}",
                        if force_full {
                            "Full Day 8:00 AM – 5:00 PM (all 8 hours filled)"
                        } else {
                            "auto-adjusted for current time"
                        },
                        list.join("\n")
                    ),
                    true,
                )
            }
            Err(e) => (format!("Could not generate rule reports: {e}"), false),
        };
        if let Some(lock) = UI.get() {
            if let Ok(mut s) = lock.lock() {
                s.pending_reply = Some(reply);
            }
        }
        PostMessageW(hwnd_main as HWND, WM_WORKER_DONE, 0, 0);
        let _ = hwnd_stop;
    });
}

#[cfg(windows)]
unsafe fn trigger_all_reports() {
    let Some(lock) = UI.get() else { return };
    let (hwnd_main, hwnd_stop, force_full) = {
        let Ok(mut s) = lock.lock() else { return };
        if s.busy {
            return;
        }
        let full = SendMessageW(s.hwnd_chk_full_day as HWND, BM_GETCHECK, 0, 0) as usize
            == BST_CHECKED;
        s.busy = true;
        EnableWindow(s.hwnd_stop as HWND, 1);
        start_thinking_timer(s.hwnd_main as HWND);
        (s.hwnd_main, s.hwnd_stop, full)
    };

    let mode_str = if force_full {
        "Full Day 8 AM – 5 PM"
    } else {
        "Auto-Adjusted"
    };
    append_message(
        "You",
        &format!("📝 [Triggered 1-Click Daily Report + Word/Excel Rules ({mode_str})]"),
    );
    let today = Local::now().date_naive();

    thread::spawn(move || {
        let rule_files = report::generate_rule_reports_with_mode(today, force_full).unwrap_or_default();
        let reply = match report::generate_daily(today) {
            Ok(path) => {
                let mut msg = format!("✅ Saved Daily Report:\n• {}\n", path.display());
                for f in rule_files {
                    msg += &format!("• {}\n", f.display());
                }
                (msg, true)
            }
            Err(e) => (format!("Could not generate report: {e}"), false),
        };
        if let Some(lock) = UI.get() {
            if let Ok(mut s) = lock.lock() {
                s.pending_reply = Some(reply);
            }
        }
        PostMessageW(hwnd_main as HWND, WM_WORKER_DONE, 0, 0);
        let _ = hwnd_stop;
    });
}

#[cfg(windows)]
unsafe fn start_thinking_timer(hwnd: HWND) {
    if let Some(lock) = UI.get() {
        if let Ok(mut s) = lock.lock() {
            s.thinking_seconds = 0;
            let status = "🧠 Thinking (0s)... (Queue: Ingesting context & recent activity)";
            SetWindowTextW(s.hwnd_status as HWND, wide(status).as_ptr());
        }
    }
    SetTimer(hwnd, IDT_THINKING_TIMER, 1000, None);
}

#[cfg(windows)]
unsafe fn on_thinking_tick(_hwnd: HWND) {
    let Some(lock) = UI.get() else { return };
    let Ok(mut s) = lock.lock() else { return };
    if !s.busy {
        return;
    }
    s.thinking_seconds += 1;
    let sec = s.thinking_seconds;
    let phase = match sec {
        0..=2 => "Ingesting context & recent activity",
        3..=5 => "Analyzing project branches & Git logs",
        6..=10 => "Synthesizing hourly causes & technical findings",
        11..=20 => "Querying connected AI model for completion",
        _ => "Finalizing structured response & markdown",
    };
    let status = format!("🧠 Thinking ({}s)... (Queue: {})", sec, phase);
    SetWindowTextW(s.hwnd_status as HWND, wide(&status).as_ptr());
}

#[cfg(windows)]
unsafe fn notify_worker_done(hwnd: HWND, elapsed: u32) {
    // 1. Taskbar Flash (flashes taskbar button to draw attention)
    FlashWindow(hwnd, 1);

    // 2. System sound alert
    MessageBeep(0);

    // 3. Native Windows system tray notification / balloon toast
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1001;
    nid.uFlags = NIF_INFO | NIF_TIP;

    let title_wide = wide("DayRecap AI Done");
    let msg_wide = wide(&format!("Response generated in {elapsed}s. Ready for your review!"));
    let tip_wide = wide("DayRecap Personal AI Secretary");

    let t_len = title_wide.len().min(nid.szInfoTitle.len());
    nid.szInfoTitle[..t_len].copy_from_slice(&title_wide[..t_len]);

    let m_len = msg_wide.len().min(nid.szInfo.len());
    nid.szInfo[..m_len].copy_from_slice(&msg_wide[..m_len]);

    let tip_len = tip_wide.len().min(nid.szTip.len());
    nid.szTip[..tip_len].copy_from_slice(&tip_wide[..tip_len]);

    nid.dwInfoFlags = NIIF_INFO;

    Shell_NotifyIconW(NIM_ADD, &nid);
    Shell_NotifyIconW(NIM_MODIFY, &nid);
}

#[cfg(windows)]
unsafe fn clear_tray_notification(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1001;
    Shell_NotifyIconW(NIM_DELETE, &nid);
}

#[cfg(windows)]
unsafe fn on_worker_done() {
    let Some(lock) = UI.get() else { return };
    let (taken, hwnd_status, hwnd_main, elapsed, hwnd_stop) = {
        let Ok(mut s) = lock.lock() else { return };
        s.busy = false;
        KillTimer(s.hwnd_main as HWND, IDT_THINKING_TIMER);
        EnableWindow(s.hwnd_stop as HWND, 0);
        let elapsed = s.thinking_seconds;
        (
            s.pending_reply.take(),
            s.hwnd_status as HWND,
            s.hwnd_main as HWND,
            elapsed,
            s.hwnd_stop as HWND,
        )
    };
    if let Some((text, open_dir)) = taken {
        if let Ok(mut s) = lock.lock() {
            s.latest_reply = text.clone();
            s.chat_history.push((false, text.clone()));
        }
        append_message("DayRecap", &text);

        notify_worker_done(hwnd_main, elapsed);

        let today = Local::now().date_naive();
        let summary = report::load_summary(today);
        let senses_count = senses::load_senses(today).len();
        let engine = report::engine_label();
        let status_text = format!(
            "🟢 Ready (took {}s)  |  Active: {}  |  Breaks: {}  |  Notes: {}  |  AI: {}  |  Auto: 4:40 PM",
            elapsed,
            util::fmt_dur(summary.active),
            util::fmt_dur(summary.idle),
            senses_count,
            engine
        );
        SetWindowTextW(hwnd_status, wide(&status_text).as_ptr());

        let h_input = HWND_INPUT.load(Ordering::Relaxed) as HWND;
        if !h_input.is_null() {
            SetFocus(h_input);
        }

        if open_dir && config::load().reports.open_after_generate {
            util::open_path(&paths::reports_dir());
        }
        let _ = hwnd_stop;
    }
}

#[cfg(windows)]
unsafe fn append_message(who: &str, text: &str) {
    let Some(lock) = UI.get() else { return };
    let (hwnd_hist, full_text) = {
        let Ok(mut s) = lock.lock() else { return };
        let clean = text.replace("\r\n", "\n").replace('\n', "\r\n");
        let entry = format!(
            "\r\n[{}] {}:\r\n{}\r\n------------------------------------------------------------\r\n",
            Local::now().format("%I:%M %p"),
            who,
            clean.trim()
        );
        s.transcript.push_str(&entry);
        (s.hwnd_history as HWND, s.transcript.clone())
    };
    let w = wide(&full_text);
    SetWindowTextW(hwnd_hist, w.as_ptr());
    let len = w.len();
    SendMessageW(hwnd_hist, EM_SETSEL, len, len as LPARAM);
    SendMessageW(hwnd_hist, EM_SCROLLCARET, 0, 0);
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_and_redo_records_at_least_10_moves() {
        let mut hist = InputHistory {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            current_text: String::new(),
            suppress_change: false,
            sent_prompts: Vec::new(),
            history_idx: None,
            saved_draft: String::new(),
        };

        // Simulate 15 consecutive editing moves
        for i in 1..=15 {
            let curr = hist.current_text.clone();
            hist.undo_stack.push(curr);
            hist.redo_stack.clear();
            hist.current_text = format!("Text state {i}");
        }

        assert_eq!(hist.current_text, "Text state 15");
        assert_eq!(hist.undo_stack.len(), 15);

        // Perform 10 consecutive undo moves
        for expected in (5..=14).rev() {
            let prev = hist.undo_stack.pop().unwrap();
            let curr = hist.current_text.clone();
            hist.redo_stack.push(curr);
            hist.current_text = prev;
            assert_eq!(hist.current_text, format!("Text state {expected}"));
        }

        assert_eq!(hist.redo_stack.len(), 10);
        assert_eq!(hist.current_text, "Text state 5");

        // Perform 10 consecutive redo moves
        for expected in 6..=15 {
            let next = hist.redo_stack.pop().unwrap();
            let curr = hist.current_text.clone();
            hist.undo_stack.push(curr);
            hist.current_text = next;
            assert_eq!(hist.current_text, format!("Text state {expected}"));
        }

        assert_eq!(hist.current_text, "Text state 15");
        assert!(hist.redo_stack.is_empty());
    }

    #[test]
    fn prompt_history_records_and_cycles_at_least_10_prompts() {
        let mut hist = InputHistory {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            current_text: String::new(),
            suppress_change: false,
            sent_prompts: Vec::new(),
            history_idx: None,
            saved_draft: String::new(),
        };

        for i in 1..=12 {
            hist.sent_prompts.push(format!("User sent message {i}"));
        }

        assert_eq!(hist.sent_prompts.len(), 12);
        // Cycle up 5 times
        let mut curr_idx = hist.sent_prompts.len() - 1;
        for i in (8..=12).rev() {
            assert_eq!(hist.sent_prompts[curr_idx], format!("User sent message {i}"));
            if curr_idx > 0 {
                curr_idx -= 1;
            }
        }
    }
}
