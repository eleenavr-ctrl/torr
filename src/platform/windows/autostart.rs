use crate::platform::{Autostart, AutostartMode, AutostartStatus};
use std::path::{Path, PathBuf};

const REG_RUN_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const REG_VALUE_NAME: &str = "TorrServerUI";

pub struct WindowsAutostart;

impl WindowsAutostart {
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

    fn clear_shortcut() -> Result<(), String> {
        if let Some(lnk) = Self::startup_shortcut_path() {
            if lnk.exists() {
                std::fs::remove_file(&lnk)
                    .map_err(|e| format!("Не удалось удалить ярлык автозапуска: {e}"))?;
            }
        }
        Ok(())
    }

    fn write_shortcut(exe_path: &Path, start_minimized: bool) -> Result<(), String> {
        let lnk = Self::startup_shortcut_path()
            .ok_or_else(|| "Не найдена папка Автозагрузки пользователя".to_string())?;
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
        let args = if start_minimized { "--minimized" } else { "" };

        let script = format!(
            "$ws = New-Object -ComObject WScript.Shell; \
             $s = $ws.CreateShortcut('{lnk_str}'); \
             $s.TargetPath = '{target}'; \
             $s.Arguments = '{args}'; \
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
                .map_err(|e| format!("Ошибка запуска PowerShell: {e}"))?;
            if !output.status.success() {
                let err = String::from_utf8_lossy(&output.stderr);
                return Err(format!("Ошибка создания ярлыка: {err}"));
            }
        }
        #[cfg(not(windows))]
        {
            let _ = script;
        }
        Ok(())
    }

    fn read_shortcut_target() -> Option<String> {
        let lnk = Self::startup_shortcut_path()?;
        if !lnk.exists() {
            return None;
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let lnk_str = lnk.to_string_lossy().replace('\'', "''");
            let script = format!(
                "(New-Object -ComObject WScript.Shell).CreateShortcut('{lnk_str}').TargetPath"
            );
            if let Ok(out) = std::process::Command::new("powershell.exe")
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
            {
                if out.status.success() {
                    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
        }
        Some(String::new())
    }

    #[cfg(windows)]
    fn read_registry_run() -> Option<String> {
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
            REG_SZ,
        };
        let subkey: Vec<u16> = format!("{REG_RUN_PATH}\0").encode_utf16().collect();
        let val_name: Vec<u16> = format!("{REG_VALUE_NAME}\0").encode_utf16().collect();
        unsafe {
            let mut hkey: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
                return None;
            }
            let mut buf = vec![0u16; 1024];
            let mut byte_len = (buf.len() * 2) as u32;
            let mut reg_type = REG_SZ;
            let st = RegQueryValueExW(
                hkey,
                val_name.as_ptr(),
                std::ptr::null(),
                &mut reg_type,
                buf.as_mut_ptr() as *mut u8,
                &mut byte_len,
            );
            RegCloseKey(hkey);
            if st == 0 {
                let u16_len = (byte_len as usize / 2).saturating_sub(1);
                return Some(String::from_utf16_lossy(&buf[..u16_len]));
            }
        }
        None
    }

    #[cfg(not(windows))]
    fn read_registry_run() -> Option<String> {
        None
    }

    #[cfg(windows)]
    fn write_registry_run(exe_path: &Path, start_minimized: bool) -> Result<(), String> {
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
            REG_OPTION_NON_VOLATILE, REG_SZ,
        };
        let subkey: Vec<u16> = format!("{REG_RUN_PATH}\0").encode_utf16().collect();
        let val_name: Vec<u16> = format!("{REG_VALUE_NAME}\0").encode_utf16().collect();
        let cmd = if start_minimized {
            format!("\"{}\" --minimized", exe_path.display())
        } else {
            format!("\"{}\"", exe_path.display())
        };
        let wide: Vec<u16> = format!("{cmd}\0").encode_utf16().collect();
        unsafe {
            let mut hkey: HKEY = std::ptr::null_mut();
            let st = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut hkey,
                std::ptr::null_mut(),
            );
            if st != 0 {
                return Err(format!("RegCreateKeyExW ошибка: {st}"));
            }
            let st2 = RegSetValueExW(
                hkey,
                val_name.as_ptr(),
                0,
                REG_SZ,
                wide.as_ptr() as *const u8,
                (wide.len() * 2) as u32,
            );
            RegCloseKey(hkey);
            if st2 != 0 {
                return Err(format!("RegSetValueExW ошибка: {st2}"));
            }
        }
        Ok(())
    }

    #[cfg(not(windows))]
    fn write_registry_run(_exe_path: &Path, _start_minimized: bool) -> Result<(), String> {
        Ok(())
    }

    #[cfg(windows)]
    fn clear_registry_run() -> Result<(), String> {
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegDeleteValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
        };
        let subkey: Vec<u16> = format!("{REG_RUN_PATH}\0").encode_utf16().collect();
        let val_name: Vec<u16> = format!("{REG_VALUE_NAME}\0").encode_utf16().collect();
        unsafe {
            let mut hkey: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_SET_VALUE, &mut hkey) == 0 {
                let _ = RegDeleteValueW(hkey, val_name.as_ptr());
                RegCloseKey(hkey);
            }
        }
        Ok(())
    }

    #[cfg(not(windows))]
    fn clear_registry_run() -> Result<(), String> {
        Ok(())
    }
}

fn normalize_exe_cmp(raw: &str) -> String {
    let trimmed = raw.trim().trim_start_matches('"');
    if let Some(end_q) = trimmed.find('"') {
        trimmed[..end_q].to_lowercase()
    } else if let Some(idx) = trimmed.find(" --") {
        trimmed[..idx].trim().to_lowercase()
    } else {
        trimmed.to_lowercase()
    }
}

impl Autostart for WindowsAutostart {
    fn status(&self, current_exe: &Path) -> AutostartStatus {
        let current_norm = current_exe.to_string_lossy().to_lowercase();
        if let Some(reg_val) = Self::read_registry_run() {
            let target_norm = normalize_exe_cmp(&reg_val);
            let stale = !target_norm.is_empty() && target_norm != current_norm;
            return AutostartStatus {
                active_mode: 2,
                needs_repair: stale,
                registered_target: reg_val,
            };
        }
        if let Some(target) = Self::read_shortcut_target() {
            let target_norm = normalize_exe_cmp(&target);
            let stale = !target_norm.is_empty() && target_norm != current_norm;
            return AutostartStatus {
                active_mode: 1,
                needs_repair: stale,
                registered_target: target,
            };
        }
        AutostartStatus::default()
    }

    fn apply(
        &self,
        mode: AutostartMode,
        current_exe: &Path,
        start_minimized: bool,
    ) -> Result<(), String> {
        match mode {
            AutostartMode::Off => {
                Self::clear_shortcut()?;
                Self::clear_registry_run()?;
                Ok(())
            }
            AutostartMode::StartupShortcut => {
                Self::clear_registry_run()?;
                Self::write_shortcut(current_exe, start_minimized)
            }
            AutostartMode::RegistryRun => {
                Self::clear_shortcut()?;
                Self::write_registry_run(current_exe, start_minimized)
            }
        }
    }

    fn repair(
        &self,
        mode: AutostartMode,
        current_exe: &Path,
        start_minimized: bool,
    ) -> Result<(), String> {
        let effective = match mode {
            AutostartMode::Off => AutostartMode::from_i32(self.status(current_exe).active_mode),
            other => other,
        };
        if effective == AutostartMode::Off {
            return Ok(());
        }
        self.apply(effective, current_exe, start_minimized)
    }
}
