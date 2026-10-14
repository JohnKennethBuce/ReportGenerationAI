//! Native Win32 System Tray daemon: creates the tray icon (`Shell_NotifyIconW` with proper
//! `cbSize`), popup context menu, and Win32 message loop.
//! Also opens the Chatbox window on startup so you immediately see DayRecap running.

use crate::{
    autostart, config, icon, paths, report, rules, scheduler,
    tracker::{self, Shared},
    util,
};
use chrono::Local;
use std::{
    ptr,
    sync::{Arc, OnceLock},
    thread,
};

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    UI::{
        Shell::{
            Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
            NOTIFYICONDATAW,
        },
        WindowsAndMessaging::{
            AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
            DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, KillTimer,
            PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer,
            TrackPopupMenu, TranslateMessage, CW_USEDEFAULT, HICON, MF_CHECKED, MF_SEPARATOR,
            MF_STRING, MF_UNCHECKED, MSG, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_NONOTIFY,
            TPM_RETURNCMD, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP,
            WM_TIMER, WM_USER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
        },
    },
};

const WM_TRAYICON: u32 = WM_USER + 101;
const TRAY_UID: u32 = 1;
const TIMER_ID: usize = 1;

const ID_CHAT: usize = 1001;
const ID_OFFICE: usize = 1002;
const ID_REPORT: usize = 1003;
const ID_FOLDER: usize = 1004;
const ID_RULES_DIR: usize = 1005;
const ID_PAUSE_30: usize = 1006;
const ID_PAUSE_DAY: usize = 1007;
const ID_RESUME: usize = 1008;
const ID_AUTOSTART: usize = 1009;
const ID_SETTINGS: usize = 1010;
const ID_QUIT: usize = 1011;

static GLOBAL_SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

pub fn run() {
    let _lock = match util::try_lock("daemon") {
        Some(l) => l,
        None => {
            // Already running in background: bring up the Chatbox window immediately!
            util::log("Daemon already running; opening Chatbox window.");
            util::spawn_chat();
            return;
        }
    };

    rules::ensure_defaults();
    util::log("Starting DayRecap tray daemon...");

    let shared = Arc::new(Shared::default());
    let _ = GLOBAL_SHARED.set(shared.clone());

    let tracker_handle = tracker::spawn(shared.clone());
    scheduler::spawn();

    // Open the Chatbox automatically on startup so the user sees DayRecap right away
    if config::load().reports.auto_morning_popup {
        util::spawn_chat();
    }

    #[cfg(windows)]
    run_win32_tray_loop(shared);

    tracker_handle.stop();
}

#[cfg(windows)]
fn run_win32_tray_loop(shared: Arc<Shared>) {
    unsafe {
        let class_name = wide("DayRecapTrayWindowClass");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class_name.as_ptr(),
            wide("DayRecap").as_ptr(),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
        );

        if hwnd.is_null() {
            util::log("CreateWindowExW failed; keeping tracker alive in background loop.");
            loop {
                thread::sleep(std::time::Duration::from_secs(60));
            }
        }

        let icon_active = create_hicon(false);
        let icon_paused = create_hicon(true);

        if add_or_modify_tray_icon(
            hwnd,
            NIM_ADD,
            icon_active,
            "DayRecap — Tracking Activity (Click to open Chatbox)",
        ) {
            util::log("Tray icon added to Windows notification area successfully.");
        } else {
            util::log("Note: Tray icon not attached (non-interactive session); background tracker active.");
        }

        SetTimer(hwnd, TIMER_ID, 1000, None);

        let mut was_paused = false;
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            if msg.message == WM_TIMER {
                let is_paused = shared.paused_until().is_some();
                if is_paused != was_paused {
                    was_paused = is_paused;
                    let ic = if is_paused { icon_paused } else { icon_active };
                    let tip = if is_paused {
                        "DayRecap — Paused"
                    } else {
                        "DayRecap — Tracking Activity (Click to open Chatbox)"
                    };
                    let _ = add_or_modify_tray_icon(hwnd, NIM_MODIFY, ic, tip);
                }
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        KillTimer(hwnd, TIMER_ID);
        remove_tray_icon(hwnd);
    }
}

#[cfg(windows)]
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAYICON => {
            let event = (lparam & 0xFFFF) as u32;
            if event == WM_LBUTTONUP || event == WM_LBUTTONDBLCLK {
                util::spawn_chat();
            } else if event == WM_RBUTTONUP {
                show_context_menu(hwnd);
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(windows)]
unsafe fn show_context_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }

    let is_paused = GLOBAL_SHARED
        .get()
        .map(|s| s.paused_until().is_some())
        .unwrap_or(false);

    append_item(menu, ID_CHAT, "💬 Open Chatbox && Rules");
    append_item(menu, ID_OFFICE, "📊 Generate Word && Excel Reports (Active Rules)");
    append_item(menu, ID_REPORT, "📝 Generate All Reports (1-Click)");
    append_item(menu, ID_FOLDER, "📂 Open Reports Folder");
    append_item(menu, ID_RULES_DIR, "📋 Open Rules Folder (.txt / .toml)");
    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());

    if is_paused {
        append_item(menu, ID_RESUME, "▶ Resume Tracking (Currently Paused)");
    } else {
        append_item(menu, ID_PAUSE_30, "⏸ Pause Tracking (30 min)");
        append_item(menu, ID_PAUSE_DAY, "🌙 Pause Until Tomorrow");
    }
    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());

    let auto_flags = if autostart::is_enabled() {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING | MF_UNCHECKED
    };
    AppendMenuW(
        menu,
        auto_flags,
        ID_AUTOSTART,
        wide("🚀 Start with Windows").as_ptr(),
    );
    append_item(menu, ID_SETTINGS, "⚙ Edit Settings (config.toml)");
    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
    append_item(menu, ID_QUIT, "✖ Quit DayRecap");

    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);

    let cmd = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_NONOTIFY | TPM_BOTTOMALIGN | TPM_LEFTALIGN,
        pt.x,
        pt.y,
        0,
        hwnd,
        ptr::null(),
    ) as usize;

    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);

    handle_menu_command(hwnd, cmd);
}

#[cfg(windows)]
unsafe fn append_item(menu: *mut core::ffi::c_void, id: usize, label: &str) {
    AppendMenuW(menu, MF_STRING, id, wide(label).as_ptr());
}

#[cfg(windows)]
unsafe fn handle_menu_command(hwnd: HWND, cmd: usize) {
    match cmd {
        ID_CHAT => util::spawn_chat(),
        ID_OFFICE => {
            thread::spawn(|| {
                let today = Local::now().date_naive();
                match report::generate_rule_reports(today) {
                    Ok(files) => {
                        for f in &files {
                            util::log(format!("Generated rule report: {}", f.display()));
                        }
                        if config::load().reports.open_after_generate {
                            util::open_path(&paths::reports_dir());
                        }
                    }
                    Err(e) => util::log(format!("Rule report error: {e}")),
                }
            });
        }
        ID_REPORT => {
            thread::spawn(|| {
                let today = Local::now().date_naive();
                match report::generate_daily(today) {
                    Ok(path) => {
                        util::log(format!("Generated daily report: {}", path.display()));
                        if config::load().reports.open_after_generate {
                            util::open_path(&path);
                        }
                    }
                    Err(err) => {
                        util::log(format!("Report generation error: {err}"));
                        util::open_path(&paths::reports_dir());
                    }
                }
            });
        }
        ID_FOLDER => util::open_path(&paths::reports_dir()),
        ID_RULES_DIR => util::open_path(&paths::rules_dir()),
        ID_PAUSE_30 => {
            if let Some(s) = GLOBAL_SHARED.get() {
                s.pause_for(30);
            }
        }
        ID_PAUSE_DAY => {
            if let Some(s) = GLOBAL_SHARED.get() {
                s.pause_until_tomorrow();
            }
        }
        ID_RESUME => {
            if let Some(s) = GLOBAL_SHARED.get() {
                s.resume();
            }
        }
        ID_AUTOSTART => {
            let next = !autostart::is_enabled();
            let _ = autostart::set(next);
        }
        ID_SETTINGS => util::open_path(&paths::config_file()),
        ID_QUIT => {
            util::log("Quitting DayRecap from tray menu.");
            DestroyWindow(hwnd);
        }
        _ => {}
    }
}

#[cfg(windows)]
unsafe fn add_or_modify_tray_icon(
    hwnd: HWND,
    action: u32,
    hicon: HICON,
    tooltip: &str,
) -> bool {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAYICON;
    nid.hIcon = hicon;

    let tip_wide = wide(tooltip);
    for (i, &ch) in tip_wide.iter().take(127).enumerate() {
        nid.szTip[i] = ch;
    }

    Shell_NotifyIconW(action, &nid) != 0
}

#[cfg(windows)]
unsafe fn remove_tray_icon(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
}

#[cfg(windows)]
unsafe fn create_hicon(paused: bool) -> HICON {
    let size = 32usize;
    let rgba = icon::rgba(size as u32, paused);
    // Convert RGBA to BGRA for Win32 CreateIcon
    let mut bgra = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        bgra.push(px[2]); // B
        bgra.push(px[1]); // G
        bgra.push(px[0]); // R
        bgra.push(px[3]); // A
    }
    let and_mask = vec![0u8; (size * size) / 8];
    CreateIcon(
        ptr::null_mut(),
        size as i32,
        size as i32,
        1,
        32,
        and_mask.as_ptr(),
        bgra.as_ptr(),
    )
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
