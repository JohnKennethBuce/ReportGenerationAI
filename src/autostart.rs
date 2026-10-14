//! "Start with Windows" toggle (HKCU\...\Run — no admin rights needed).
//! macOS/Linux support is planned for Phase 5.

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::{
        Foundation::ERROR_SUCCESS,
        System::Registry::{
            RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
        },
    };

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "DayRecap";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn is_enabled() -> bool {
        let (key, val) = (wide(RUN_KEY), wide(VALUE));
        let mut size = 0u32;
        // SAFETY: valid null-terminated strings; we only query the size.
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                val.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            ) == ERROR_SUCCESS
        }
    }

    pub fn set(enabled: bool) -> Result<(), String> {
        let (key, val) = (wide(RUN_KEY), wide(VALUE));
        let status = if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let data = wide(&format!("\"{}\"", exe.display()));
            // SAFETY: data is a null-terminated UTF-16 buffer; size is in bytes.
            unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    key.as_ptr(),
                    val.as_ptr(),
                    REG_SZ,
                    data.as_ptr().cast(),
                    (data.len() * 2) as u32,
                )
            }
        } else {
            // SAFETY: valid null-terminated strings.
            unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr()) }
        };
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("registry error {status}"))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn is_enabled() -> bool {
        false
    }
    pub fn set(_enabled: bool) -> Result<(), String> {
        Err("autostart is not supported on this OS yet".into())
    }
}

pub use imp::{is_enabled, set};
