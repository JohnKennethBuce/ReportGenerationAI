//! OS-specific probes: which window is in front, and how long the user has been idle.
//! Only window *titles* and *process names* are read — never keystrokes or screen content.

pub struct Foreground {
    /// Executable name, e.g. `Code.exe`
    pub app: String,
    /// Window title, e.g. `main.rs - ReportGenerationAI - Visual Studio Code`
    pub title: String,
}

#[cfg(windows)]
mod imp {
    use super::Foreground;
    use std::path::Path;
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::{
            SystemInformation::GetTickCount,
            Threading::{
                OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
        UI::{
            Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
            WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId},
        },
    };

    pub fn foreground() -> Option<Foreground> {
        // SAFETY: plain Win32 calls with correctly sized, stack-owned buffers.
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return None;
            }
            let mut buf = [0u16; 512];
            let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
            let title = String::from_utf16_lossy(&buf[..len.max(0) as usize]);

            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            let app = process_name(pid).unwrap_or_else(|| "unknown".into());
            Some(Foreground { app, title })
        }
    }

    unsafe fn process_name(pid: u32) -> Option<String> {
        if pid == 0 {
            return None;
        }
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None; // e.g. elevated process while we run unelevated
        }
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..size as usize]);
        Path::new(&full)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    }

    pub fn idle_seconds() -> u64 {
        // SAFETY: LASTINPUTINFO is initialised with its correct size.
        unsafe {
            let mut info = LASTINPUTINFO {
                cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
                dwTime: 0,
            };
            if GetLastInputInfo(&mut info) == 0 {
                return 0;
            }
            (GetTickCount().wrapping_sub(info.dwTime) / 1000) as u64
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Foreground;

    pub fn foreground() -> Option<Foreground> {
        let w = active_win_pos_rs::get_active_window().ok()?;
        Some(Foreground {
            app: w.app_name,
            title: w.title,
        })
    }

    pub fn idle_seconds() -> u64 {
        user_idle::UserIdle::get_time()
            .map(|t| t.as_seconds())
            .unwrap_or(0)
    }
}

pub use imp::{foreground, idle_seconds};
