use crate::platform::SystemTheme;

pub struct WindowsSystemTheme;

impl SystemTheme for WindowsSystemTheme {
    fn is_dark(&self) -> bool {
        detect_windows_dark_mode()
    }

    fn accent_rgb(&self) -> (u8, u8, u8) {
        detect_windows_accent_rgb()
    }

    fn apply_window_theme(&self, window_handle: isize, is_dark: bool) {
        apply_dwm_dark_mode(window_handle, is_dark);
    }
}

pub fn detect_windows_dark_mode() -> bool {
    #[cfg(windows)]
    {
        read_hkcu_dword(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
            "AppsUseLightTheme",
        )
        .map(|v| v == 0)
        .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn detect_windows_accent_rgb() -> (u8, u8, u8) {
    #[cfg(windows)]
    {
        if let Some(argb) = read_hkcu_dword("Software\\Microsoft\\Windows\\DWM", "ColorizationColor") {
            let r = ((argb >> 16) & 0xFF) as u8;
            let g = ((argb >> 8) & 0xFF) as u8;
            let b = (argb & 0xFF) as u8;
            if r != 0 || g != 0 || b != 0 {
                return (r, g, b);
            }
        }
    }
    (0, 103, 192)
}

pub fn apply_dwm_dark_mode(hwnd: isize, is_dark: bool) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};

        if hwnd == 0 {
            return;
        }
        let dark_val: i32 = if is_dark { 1 } else { 0 };
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd as HWND,
                DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                &dark_val as *const i32 as *const core::ffi::c_void,
                std::mem::size_of::<i32>() as u32,
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (hwnd, is_dark);
    }
}

#[cfg(windows)]
fn read_hkcu_dword(path: &str, name: &str) -> Option<u32> {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD,
    };
    let subkey: Vec<u16> = format!("{path}\0").encode_utf16().collect();
    let val_name: Vec<u16> = format!("{name}\0").encode_utf16().collect();
    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) == 0 {
            let mut data: u32 = 0;
            let mut size: u32 = std::mem::size_of::<u32>() as u32;
            let mut reg_type: u32 = REG_DWORD;
            let st = RegQueryValueExW(
                hkey,
                val_name.as_ptr(),
                std::ptr::null(),
                &mut reg_type,
                &mut data as *mut u32 as *mut u8,
                &mut size,
            );
            RegCloseKey(hkey);
            if st == 0 {
                return Some(data);
            }
        }
    }
    None
}
