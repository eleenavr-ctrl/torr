pub mod autostart;
pub mod dpapi;
pub mod metrics;
pub mod single_instance;
pub mod theme;
pub mod tray;
pub mod window_hooks;

pub fn open_url_in_browser(url: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .creation_flags(0x08000000)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = url;
    }
}

pub fn apply_process_priority(above_normal: bool) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, SetPriorityClass, ABOVE_NORMAL_PRIORITY_CLASS,
            NORMAL_PRIORITY_CLASS,
        };
        let class = if above_normal {
            ABOVE_NORMAL_PRIORITY_CLASS
        } else {
            NORMAL_PRIORITY_CLASS
        };
        unsafe {
            let _ = SetPriorityClass(GetCurrentProcess(), class);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = above_normal;
    }
}
