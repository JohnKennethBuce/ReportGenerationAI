//! Small shared helpers: logging, opening files, formatting, single-instance locks.

use crate::paths;
use chrono::Local;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    process::Command,
};

/// Append a line to `dayrecap.log` (the app has no console). Rotates at ~1 MB.
pub fn log(msg: impl AsRef<str>) {
    let path = paths::log_file();
    if fs::metadata(&path).map(|m| m.len() > 1_000_000).unwrap_or(false) {
        let _ = fs::remove_file(&path);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} {}", Local::now().format("%Y-%m-%d %H:%M:%S"), msg.as_ref());
    }
}

/// Open a file or folder with the OS default application.
pub fn open_path(path: &Path) {
    #[cfg(windows)]
    let result = Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = Command::new("xdg-open").arg(path).spawn();

    if let Err(e) = result {
        log(format!("could not open {}: {e}", path.display()));
    }
}

/// Launch the chat window as a separate, short-lived process.
#[cfg(windows)]
pub fn spawn_chat() {
    use std::os::windows::process::CommandExt;
    match std::env::current_exe() {
        Ok(exe) => {
            if let Err(e) = Command::new(exe).arg("--chat").creation_flags(0x08000000).spawn() {
                log(format!("could not start chat window: {e}"));
            }
        }
        Err(e) => log(format!("current_exe failed: {e}")),
    }
}

#[cfg(not(windows))]
pub fn spawn_chat() {
    match std::env::current_exe() {
        Ok(exe) => {
            if let Err(e) = Command::new(exe).arg("--chat").spawn() {
                log(format!("could not start chat window: {e}"));
            }
        }
        Err(e) => log(format!("current_exe failed: {e}")),
    }
}

/// Try to acquire a single-instance lock.
/// Returns a lock token if this is the only instance, or None if already running.
#[cfg(windows)]
pub fn try_lock(name: &str) -> Option<usize> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, SetLastError, ERROR_ALREADY_EXISTS,
    };
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let wide_name: Vec<u16> = format!("Local\\DayRecap_{name}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        SetLastError(0);
        let handle = CreateMutexW(std::ptr::null_mut(), 1, wide_name.as_ptr());
        let err = GetLastError();
        if handle.is_null() || err == ERROR_ALREADY_EXISTS {
            if !handle.is_null() {
                CloseHandle(handle);
            }
            None
        } else {
            Some(handle as usize)
        }
    }
}

#[cfg(not(windows))]
pub fn try_lock(_name: &str) -> Option<usize> {
    Some(1)
}

/// 42 -> "42s", 600 -> "10m", 3900 -> "1h 05m"
pub fn fmt_dur(secs: i64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        format!("{mins}m")
    } else {
        format!("{}h {:02}m", mins / 60, mins % 60)
    }
}

/// Truncate to `max` characters (not bytes), adding an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}
