pub mod diagnostics;
pub mod storage;
pub mod tray;

use crate::AppWindow;
use slint::ComponentHandle;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Generates a crisp 32x32 Windows 11 style blue-cyan rounded-square icon with a play glyph.
pub fn generate_app_icon_rgba() -> Vec<u8> {
    const W: usize = 32;
    const H: usize = 32;
    let mut buf = vec![0u8; W * H * 4];

    for y in 0..H {
        for x in 0..W {
            let idx = (y * W + x) * 4;
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;

            // Rounded rect SDF (center 16,16, half-size 14, corner radius 6)
            let dx = (fx - 16.0).abs() - 8.0;
            let dy = (fy - 16.0).abs() - 8.0;
            let ox = dx.max(0.0);
            let oy = dy.max(0.0);
            let dist = (ox * ox + oy * oy).sqrt() + dx.max(dy).min(0.0) - 6.0;

            if dist <= 0.6 {
                let alpha = (0.6 - dist).clamp(0.0, 1.0);
                let t = ((fx + fy) / 64.0).clamp(0.0, 1.0);
                // Fluent Blue (#0067C0) -> Cyan (#32B5FF)
                let mut r = (0.0 + t * 50.0) as f32;
                let mut g = (103.0 + t * 88.0) as f32;
                let mut b = (192.0 + t * 63.0) as f32;

                // Subtle top inner border highlight
                if dist > -1.2 && fy < 16.0 {
                    r = (r + 35.0).min(255.0);
                    g = (g + 35.0).min(255.0);
                    b = (b + 35.0).min(255.0);
                }

                // Stylized Play triangle inside (vertices roughly (12, 9.5), (12, 22.5), (23, 16))
                let in_play = if fx >= 11.5 && fx <= 22.8 {
                    let progress = (fx - 11.5) / (22.8 - 11.5);
                    let half_h = 6.6 * (1.0 - progress);
                    (fy - 16.0).abs() <= half_h
                } else {
                    false
                };

                if in_play {
                    r = 255.0;
                    g = 255.0;
                    b = 255.0;
                }

                buf[idx] = r.clamp(0.0, 255.0) as u8;
                buf[idx + 1] = g.clamp(0.0, 255.0) as u8;
                buf[idx + 2] = b.clamp(0.0, 255.0) as u8;
                buf[idx + 3] = (alpha * 255.0) as u8;
            }
        }
    }

    buf
}

pub fn slint_app_icon(rgba: &[u8]) -> slint::Image {
    let pixel_buf =
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(rgba, 32, 32);
    slint::Image::from_rgba8(pixel_buf)
}

pub fn detect_windows_dark_mode() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
            REG_DWORD,
        };

        let subkey: Vec<u16> =
            "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
                .encode_utf16()
                .collect();
        let value_name: Vec<u16> = "AppsUseLightTheme\0".encode_utf16().collect();

        unsafe {
            let mut hkey: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                KEY_READ,
                &mut hkey,
            ) == 0
            {
                let mut data: u32 = 1;
                let mut size: u32 = std::mem::size_of::<u32>() as u32;
                let mut reg_type: u32 = REG_DWORD;
                let status = RegQueryValueExW(
                    hkey,
                    value_name.as_ptr(),
                    std::ptr::null(),
                    &mut reg_type,
                    &mut data as *mut u32 as *mut u8,
                    &mut size,
                );
                RegCloseKey(hkey);
                if status == 0 {
                    return data == 0;
                }
            }
        }
        false
    }

    #[cfg(not(windows))]
    {
        false
    }
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

fn startup_shortcut_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|appdata| {
        PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup")
            .join("TorrServerUI.lnk")
    })
}

pub fn is_autostart_enabled() -> bool {
    startup_shortcut_path()
        .map(|p| p.exists())
        .unwrap_or(false)
}

pub fn set_autostart(enabled: bool, exe_path: &Path) -> Result<(), String> {
    let lnk = startup_shortcut_path()
        .ok_or_else(|| "Не найдена папка Автозагрузки пользователя".to_string())?;

    if !enabled {
        if lnk.exists() {
            std::fs::remove_file(&lnk)
                .map_err(|e| format!("Не удалось удалить ярлык автозапуска: {e}"))?;
        }
        diagnostics::info("[CORE]", "Автозапуск отключён (ярлык удалён)");
        return Ok(());
    }

    if let Some(parent) = lnk.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let work_dir = exe_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let target = exe_path.to_string_lossy().replace('\'', "''");
    let lnk_str = lnk.to_string_lossy().replace('\'', "''");
    let work_esc = work_dir.replace('\'', "''");

    let script = format!(
        "$ws = New-Object -ComObject WScript.Shell; \
         $s = $ws.CreateShortcut('{lnk_str}'); \
         $s.TargetPath = '{target}'; \
         $s.WorkingDirectory = '{work_esc}'; \
         $s.Description = 'TorrServerUI Portable'; \
         $s.Save()"
    );

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                &script,
            ])
            .creation_flags(0x08000000)
            .output()
            .map_err(|e| format!("Ошибка запуска PowerShell для ярлыка: {e}"))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Не удалось создать ярлык автозапуска: {err}"));
        }
    }

    #[cfg(not(windows))]
    {
        let _ = script;
    }

    diagnostics::info("[CORE]", "Автозапуск включён (ярлык создан в Startup)");
    Ok(())
}

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

pub fn show_toast(ui: &AppWindow, message: impl Into<String>) {
    let msg = message.into();
    ui.set_toast_message(msg.into());
    ui.set_toast_visible(true);
    let weak = ui.as_weak();
    slint::Timer::single_shot(Duration::from_secs(3), move || {
        if let Some(app) = weak.upgrade() {
            app.set_toast_visible(false);
        }
    });
}
