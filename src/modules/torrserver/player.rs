use crate::core::diagnostics;
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct DetectedPlayer {
    pub id: String,
    pub name: String,
    pub exe_path: PathBuf,
}

pub fn detect_installed_players() -> Vec<DetectedPlayer> {
    let mut found: Vec<DetectedPlayer> = Vec::new();
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".to_string());
    let pf86 = std::env::var("ProgramFiles(x86)")
        .unwrap_or_else(|_| "C:\\Program Files (x86)".to_string());
    let pf64 = std::env::var("ProgramW6432").unwrap_or_else(|_| pf.clone());
    let local_app = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    let user_profile = std::env::var("USERPROFILE").unwrap_or_default();
    let choco_bin = std::env::var("ChocolateyInstall")
        .map(|p| PathBuf::from(p).join("bin"))
        .unwrap_or_else(|_| PathBuf::from("C:\\ProgramData\\chocolatey\\bin"));

    let uninstall_entries = collect_uninstall_exe_candidates();
    let local_programs_dir = (!local_app.is_empty()).then(|| PathBuf::from(&local_app).join("Programs"));
    let winget_dir = (!local_app.is_empty())
        .then(|| PathBuf::from(&local_app).join("Microsoft\\WinGet\\Packages"));

    let candidates: Vec<(&str, &str, Vec<&str>, Vec<PathBuf>)> = vec![
        (
            "potplayer",
            "PotPlayer",
            vec!["PotPlayerMini64.exe", "PotPlayerMini.exe", "PotPlayer64.exe", "PotPlayer.exe"],
            vec![
                PathBuf::from(&pf).join("DAUM\\PotPlayer\\PotPlayerMini64.exe"),
                PathBuf::from(&pf).join("DAUM\\PotPlayer\\PotPlayer64.exe"),
                PathBuf::from(&pf64).join("DAUM\\PotPlayer\\PotPlayerMini64.exe"),
                PathBuf::from(&pf86).join("DAUM\\PotPlayer\\PotPlayerMini.exe"),
                PathBuf::from(&pf).join("PotPlayer\\PotPlayerMini64.exe"),
                PathBuf::from(&pf).join("PotPlayer\\PotPlayer64.exe"),
                PathBuf::from(&pf86).join("PotPlayer\\PotPlayerMini.exe"),
                PathBuf::from(&local_app).join("PotPlayer\\PotPlayerMini64.exe"),
                PathBuf::from(&local_app).join("Programs\\PotPlayer\\PotPlayerMini64.exe"),
                PathBuf::from(&user_profile).join("scoop\\apps\\potplayer\\current\\PotPlayerMini64.exe"),
            ],
        ),
        (
            "mpv",
            "MPV",
            vec!["mpv.exe", "mpvnet.exe"],
            vec![
                PathBuf::from(&pf).join("mpv\\mpv.exe"),
                PathBuf::from(&pf64).join("mpv\\mpv.exe"),
                PathBuf::from(&pf86).join("mpv\\mpv.exe"),
                PathBuf::from(&local_app).join("Programs\\mpv\\mpv.exe"),
                PathBuf::from(&appdata).join("mpv\\mpv.exe"),
                PathBuf::from(&pf).join("mpv.net\\mpvnet.exe"),
                PathBuf::from(&local_app).join("Programs\\mpv.net\\mpvnet.exe"),
                PathBuf::from(&local_app).join("Microsoft\\WindowsApps\\mpv.exe"),
                PathBuf::from(&user_profile).join("scoop\\apps\\mpv\\current\\mpv.exe"),
                choco_bin.join("mpv.exe"),
            ],
        ),
        (
            "vlc",
            "VLC Media Player",
            vec!["vlc.exe"],
            vec![
                PathBuf::from(&pf).join("VideoLAN\\VLC\\vlc.exe"),
                PathBuf::from(&pf64).join("VideoLAN\\VLC\\vlc.exe"),
                PathBuf::from(&pf86).join("VideoLAN\\VLC\\vlc.exe"),
                PathBuf::from(&local_app).join("Programs\\VideoLAN\\VLC\\vlc.exe"),
                PathBuf::from(&local_app).join("Microsoft\\WindowsApps\\vlc.exe"),
                PathBuf::from(&user_profile).join("scoop\\apps\\vlc\\current\\vlc.exe"),
                choco_bin.join("vlc.exe"),
            ],
        ),
        (
            "mpc-be",
            "MPC-BE",
            vec!["mpc-be64.exe", "mpc-be.exe"],
            vec![
                PathBuf::from(&pf).join("MPC-BE x64\\mpc-be64.exe"),
                PathBuf::from(&pf64).join("MPC-BE x64\\mpc-be64.exe"),
                PathBuf::from(&pf).join("MPC-BE\\mpc-be64.exe"),
                PathBuf::from(&pf86).join("MPC-BE\\mpc-be.exe"),
                PathBuf::from(&local_app).join("Programs\\MPC-BE\\mpc-be64.exe"),
                PathBuf::from(&user_profile).join("scoop\\apps\\mpc-be\\current\\mpc-be64.exe"),
            ],
        ),
        (
            "mpc-hc",
            "MPC-HC",
            vec!["mpc-hc64.exe", "mpc-hc.exe"],
            vec![
                PathBuf::from(&pf).join("MPC-HC\\mpc-hc64.exe"),
                PathBuf::from(&pf64).join("MPC-HC\\mpc-hc64.exe"),
                PathBuf::from(&pf86).join("MPC-HC\\mpc-hc.exe"),
                PathBuf::from(&pf).join("K-Lite Codec Pack\\MPC-HC64\\mpc-hc64.exe"),
                PathBuf::from(&pf86).join("K-Lite Codec Pack\\MPC-HC64\\mpc-hc64.exe"),
                PathBuf::from(&pf86).join("K-Lite Codec Pack\\MPC-HC\\mpc-hc.exe"),
                PathBuf::from(&local_app).join("Programs\\MPC-HC\\mpc-hc64.exe"),
                PathBuf::from(&user_profile).join("scoop\\apps\\mpc-hc\\current\\mpc-hc64.exe"),
            ],
        ),
        (
            "kmplayer",
            "KMPlayer",
            vec!["KMPlayer64.exe", "KMPlayer.exe"],
            vec![
                PathBuf::from(&pf).join("KMPlayer 64X\\KMPlayer64.exe"),
                PathBuf::from(&pf86).join("The KMPlayer\\KMPlayer.exe"),
                PathBuf::from("C:\\KMPlayer\\KMPlayer.exe"),
            ],
        ),
        (
            "smplayer",
            "SMPlayer",
            vec!["smplayer.exe"],
            vec![
                PathBuf::from(&pf).join("SMPlayer\\smplayer.exe"),
                PathBuf::from(&pf86).join("SMPlayer\\smplayer.exe"),
            ],
        ),
        (
            "kodi",
            "Kodi",
            vec!["kodi.exe"],
            vec![
                PathBuf::from(&pf).join("Kodi\\kodi.exe"),
                PathBuf::from(&pf86).join("Kodi\\kodi.exe"),
            ],
        ),
        (
            "light-alloy",
            "Light Alloy",
            vec!["LA.exe"],
            vec![
                PathBuf::from(&pf).join("Light Alloy\\LA.exe"),
                PathBuf::from(&pf86).join("Light Alloy\\LA.exe"),
            ],
        ),
        (
            "gom",
            "GOM Player",
            vec!["GOM64.exe", "GOM.exe"],
            vec![
                PathBuf::from(&pf).join("GOM\\GOMPlayerPlus\\GOM64.exe"),
                PathBuf::from(&pf86).join("GRETECH\\GOMPlayer\\GOM.exe"),
            ],
        ),
        (
            "ace-player",
            "Ace Player HD",
            vec!["ace_player.exe"],
            vec![
                PathBuf::from(&appdata).join("ACEStream\\player\\ace_player.exe"),
                PathBuf::from(&pf86).join("ACEStream\\player\\ace_player.exe"),
            ],
        ),
        (
            "jellyfin",
            "Jellyfin Media Player",
            vec!["JellyfinMediaPlayer.exe"],
            vec![
                PathBuf::from(&pf).join("Jellyfin\\Jellyfin Media Player\\JellyfinMediaPlayer.exe"),
            ],
        ),
    ];

    for (id, name, exe_names, paths) in candidates {
        let resolved = paths
            .into_iter()
            .find(|p| p.is_file())
            .or_else(|| exe_names.iter().find_map(|exe| find_in_app_paths(exe)))
            .or_else(|| match_uninstall_candidate(&uninstall_entries, &exe_names))
            .or_else(|| {
                local_programs_dir
                    .as_deref()
                    .and_then(|d| find_exe_shallow(d, &exe_names, 2))
            })
            .or_else(|| {
                winget_dir
                    .as_deref()
                    .and_then(|d| find_exe_shallow(d, &exe_names, 3))
            })
            .or_else(|| exe_names.iter().find_map(|exe| find_in_path(exe)));

        if let Some(p) = resolved {
            push_unique_player(&mut found, id, name, p);
        }
    }

    // Also inspect Windows default file associations for .mkv / .mp4 / .avi / .m3u
    for assoc_exe in find_windows_video_associations() {
        let already_known = found.iter().any(|p| paths_equal(&p.exe_path, &assoc_exe));
        if !already_known && !is_ignored_assoc_binary(&assoc_exe) {
            let stem = assoc_exe
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("system-player");
            let id = format!("assoc-{}", stem.to_lowercase());
            let label = format!("{stem} (по умолчанию в Windows)");
            push_unique_player(&mut found, &id, &label, assoc_exe);
        }
    }

    // Built-in Windows Media Player fallback (always present on standard Windows 10/11)
    let wmp_candidates = [
        PathBuf::from(&pf86).join("Windows Media Player\\wmplayer.exe"),
        PathBuf::from(&pf).join("Windows Media Player\\wmplayer.exe"),
        PathBuf::from(&pf64).join("Windows Media Player\\wmplayer.exe"),
    ];
    if let Some(wmp) = wmp_candidates.into_iter().find(|p| p.is_file()) {
        push_unique_player(&mut found, "wmplayer", "Windows Media Player", wmp);
    }

    found
}

fn push_unique_player(list: &mut Vec<DetectedPlayer>, id: &str, name: &str, exe_path: PathBuf) {
    if list.iter().any(|p| paths_equal(&p.exe_path, &exe_path)) {
        return;
    }
    list.push(DetectedPlayer {
        id: id.to_string(),
        name: name.to_string(),
        exe_path,
    });
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

fn is_ignored_assoc_binary(path: &Path) -> bool {
    let fname = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(
        fname.as_str(),
        "explorer.exe"
            | "rundll32.exe"
            | "cmd.exe"
            | "powershell.exe"
            | "openwith.exe"
            | "launchwinapp.exe"
            | "msedge.exe"
            | "chrome.exe"
            | "firefox.exe"
            | "opera.exe"
            | "brave.exe"
            | "browser.exe"
            | "notepad.exe"
            | "wmplayer.exe"
            | "unins000.exe"
            | "uninstall.exe"
            | "setup.exe"
    )
}

fn find_exe_shallow(root: &Path, exe_names: &[&str], max_depth: usize) -> Option<PathBuf> {
    if max_depth == 0 || !root.is_dir() {
        return None;
    }
    for exe in exe_names {
        let direct = root.join(exe);
        if direct.is_file() {
            return Some(direct);
        }
    }
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten().take(80) {
        let p = entry.path();
        if p.is_dir() {
            if let Some(hit) = find_exe_shallow(&p, exe_names, max_depth - 1) {
                return Some(hit);
            }
        }
    }
    None
}

fn match_uninstall_candidate(entries: &[(Option<PathBuf>, Option<PathBuf>)], exe_names: &[&str]) -> Option<PathBuf> {
    for (icon_exe, install_dir) in entries {
        if let Some(exe) = icon_exe {
            if let Some(name) = exe.file_name().and_then(|n| n.to_str()) {
                if exe_names.iter().any(|target| target.eq_ignore_ascii_case(name)) && exe.is_file() {
                    return Some(exe.clone());
                }
            }
        }
        if let Some(dir) = install_dir {
            if dir.is_dir() {
                for target in exe_names {
                    let cand = dir.join(target);
                    if cand.is_file() {
                        return Some(cand);
                    }
                    let nested = dir.join("MPC-HC64").join(target);
                    if nested.is_file() {
                        return Some(nested);
                    }
                }
            }
        }
    }
    None
}

fn collect_uninstall_exe_candidates() -> Vec<(Option<PathBuf>, Option<PathBuf>)> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER,
            HKEY_LOCAL_MACHINE, KEY_READ,
        };

        let roots: &[(HKEY, &str)] = &[
            (
                HKEY_LOCAL_MACHINE,
                "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
            ),
            (
                HKEY_LOCAL_MACHINE,
                "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
            ),
            (
                HKEY_CURRENT_USER,
                "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
            ),
        ];

        let mut out = Vec::new();
        for &(root, sub) in roots {
            let sub_w: Vec<u16> = sub.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                let mut hkey: HKEY = std::ptr::null_mut();
                if RegOpenKeyExW(root, sub_w.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
                    continue;
                }
                for idx in 0..512u32 {
                    let mut name_buf = [0u16; 260];
                    let mut name_len: u32 = name_buf.len() as u32;
                    let rc = RegEnumKeyExW(
                        hkey,
                        idx,
                        name_buf.as_mut_ptr(),
                        &mut name_len,
                        std::ptr::null(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    );
                    if rc != 0 {
                        break;
                    }
                    let sub_name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
                    let full_sub = format!("{sub}\\{sub_name}");
                    let icon_exe = reg_read_string(root, &full_sub, Some("DisplayIcon"))
                        .and_then(|s| extract_exe_path_from_command(&s));
                    let install_dir = reg_read_string(root, &full_sub, Some("InstallLocation"))
                        .map(|s| PathBuf::from(expand_env_vars(s.trim().trim_matches('"'))))
                        .filter(|p| p.is_dir());
                    if icon_exe.is_some() || install_dir.is_some() {
                        out.push((icon_exe, install_dir));
                    }
                }
                RegCloseKey(hkey);
            }
        }
        out
    }

    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

fn find_windows_video_associations() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

        let mut results: Vec<PathBuf> = Vec::new();
        for ext in [".mkv", ".mp4", ".avi", ".m3u"] {
            let mut prog_ids: Vec<String> = Vec::new();
            let uc_key = format!(
                "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\{ext}\\UserChoice"
            );
            if let Some(pid) = reg_read_string(HKEY_CURRENT_USER, &uc_key, Some("ProgId")) {
                if !pid.trim().is_empty() {
                    prog_ids.push(pid.trim().to_string());
                }
            }
            for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
                let cls_ext = format!("Software\\Classes\\{ext}");
                if let Some(pid) = reg_read_string(root, &cls_ext, None) {
                    let trimmed = pid.trim();
                    if !trimmed.is_empty() && !prog_ids.iter().any(|x| x == trimmed) {
                        prog_ids.push(trimmed.to_string());
                    }
                }
            }

            for pid in prog_ids {
                for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
                    let cmd_key = format!("Software\\Classes\\{pid}\\shell\\open\\command");
                    if let Some(cmd_raw) = reg_read_string(root, &cmd_key, None) {
                        if let Some(exe) = extract_exe_path_from_command(&cmd_raw) {
                            if !results.iter().any(|p| paths_equal(p, &exe)) {
                                results.push(exe);
                            }
                        }
                    }
                }
            }
        }
        results
    }

    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

#[cfg(windows)]
fn reg_read_string(root: windows_sys::Win32::System::Registry::HKEY, subkey: &str, value_name: Option<&str>) -> Option<String> {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, KEY_READ, REG_EXPAND_SZ, REG_SZ,
    };

    let sub_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let val_w: Option<Vec<u16>> =
        value_name.map(|v| v.encode_utf16().chain(std::iter::once(0)).collect());
    let val_ptr = val_w
        .as_ref()
        .map(|v| v.as_ptr())
        .unwrap_or(std::ptr::null());

    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(root, sub_w.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
            return None;
        }
        let mut reg_type: u32 = 0;
        let mut byte_len: u32 = 0;
        let mut out = None;
        if RegQueryValueExW(
            hkey,
            val_ptr,
            std::ptr::null(),
            &mut reg_type,
            std::ptr::null_mut(),
            &mut byte_len,
        ) == 0
            && byte_len >= 2
            && (reg_type == REG_SZ || reg_type == REG_EXPAND_SZ)
        {
            let mut buf = vec![0u16; (byte_len as usize / 2) + 1];
            if RegQueryValueExW(
                hkey,
                val_ptr,
                std::ptr::null(),
                &mut reg_type,
                buf.as_mut_ptr() as *mut u8,
                &mut byte_len,
            ) == 0
            {
                let raw = String::from_utf16_lossy(&buf)
                    .trim_end_matches('\0')
                    .trim()
                    .to_string();
                if !raw.is_empty() {
                    out = Some(raw);
                }
            }
        }
        RegCloseKey(hkey);
        out
    }
}

#[allow(dead_code)]
fn extract_exe_path_from_command(raw: &str) -> Option<PathBuf> {
    let expanded = expand_env_vars(raw.trim());
    let candidate = if let Some(after_q) = expanded.strip_prefix('"') {
        after_q.split('"').next().unwrap_or("")
    } else if let Some(idx) = expanded.to_lowercase().find(".exe") {
        &expanded[..idx + 4]
    } else {
        expanded.split(',').next().unwrap_or(expanded.as_str())
    };
    let clean = candidate.trim().trim_matches('"');
    if !clean.to_lowercase().ends_with(".exe") {
        return None;
    }
    let p = PathBuf::from(clean);
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

fn find_in_app_paths(exe_name: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

        let subkey_str = format!(
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{exe_name}"
        );

        for root_key in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            if let Some(raw) = reg_read_string(root_key, &subkey_str, None) {
                if let Some(p) = extract_exe_path_from_command(&raw) {
                    return Some(p);
                }
                let p = PathBuf::from(expand_env_vars(raw.trim().trim_matches('"')));
                if p.is_file() {
                    return Some(p);
                }
            }
        }
        None
    }

    #[cfg(not(windows))]
    {
        let _ = exe_name;
        None
    }
}

#[allow(dead_code)]
fn expand_env_vars(input: &str) -> String {
    if !input.contains('%') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after_start = &rest[start + 1..];
        if let Some(end) = after_start.find('%') {
            let var_name = &after_start[..end];
            if !var_name.is_empty() {
                if let Ok(val) = std::env::var(var_name) {
                    out.push_str(&val);
                } else {
                    out.push('%');
                    out.push_str(var_name);
                    out.push('%');
                }
            }
            rest = &after_start[end + 1..];
        } else {
            out.push('%');
            rest = after_start;
            break;
        }
    }
    out.push_str(rest);
    out
}

fn find_in_path(exe_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(exe_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub fn resolve_player_exe(
    custom_path: &str,
    selected_idx: usize,
    detected: &[DetectedPlayer],
) -> Result<PathBuf, String> {
    let custom = custom_path.trim();
    if !custom.is_empty() {
        let p = PathBuf::from(custom);
        if p.exists() {
            return Ok(p);
        }
        return Err(format!("Указанный плеер не найден: {}", p.display()));
    }

    if let Some(player) = detected.get(selected_idx) {
        if player.exe_path.exists() {
            return Ok(player.exe_path.clone());
        }
    }
    if let Some(first) = detected.first() {
        if first.exe_path.exists() {
            return Ok(first.exe_path.clone());
        }
    }

    Err("Не найден видеоплеер. Установите PotPlayer/MPV/VLC или укажите путь к .exe в Настройках.".to_string())
}

pub fn launch_player(player_exe: &Path, target: &str, extra_args: &str) -> Result<(), String> {
    let mut cmd = Command::new(player_exe);
    for arg in split_args(extra_args) {
        cmd.arg(arg);
    }
    cmd.arg(target);

    cmd.spawn()
        .map_err(|e| format!("Не удалось запустить плеер {}: {e}", player_exe.display()))?;

    diagnostics::info(
        "[TS:PLAYER]",
        format!("Запуск {} -> {}", player_exe.display(), target),
    );
    Ok(())
}

pub fn open_with_os_default(target: &Path) -> Result<(), String> {
    let path_str = target.to_string_lossy().to_string();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        Command::new("cmd")
            .args(["/C", "start", "", &path_str])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .spawn()
            .map_err(|e| format!("Не удалось открыть плейлист в системе: {e}"))?;
    }
    #[cfg(not(windows))]
    {
        Command::new("xdg-open")
            .arg(&path_str)
            .spawn()
            .map_err(|e| format!("Не удалось открыть плейлист в системе: {e}"))?;
    }
    diagnostics::info(
        "[TS:PLAYER]",
        format!("Открытие через системный обработчик Windows -> {path_str}"),
    );
    Ok(())
}

fn split_args(raw: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for ch in raw.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ' ' | '\t' if !in_quotes => {
                if !cur.is_empty() {
                    args.push(cur.clone());
                    cur.clear();
                }
            }
            _ => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        args.push(cur);
    }
    args
}

pub fn write_m3u_playlist(
    playlists_dir: &Path,
    hash: &str,
    entries: &[(String, String)],
) -> Result<PathBuf, String> {
    let safe_hash: String = hash
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let file_name = if safe_hash.is_empty() {
        "playlist.m3u".to_string()
    } else {
        format!("{safe_hash}.m3u")
    };
    let path = playlists_dir.join(file_name);

    let mut content = String::from("#EXTM3U\n");
    for (title, url) in entries {
        content.push_str(&format!("#EXTINF:-1,{title}\n{url}\n"));
    }

    fs::write(&path, content)
        .map_err(|e| format!("Ошибка создания .m3u плейлиста: {e}"))?;
    Ok(path)
}

pub fn is_video_filename(name: &str) -> bool {
    let low = name.to_lowercase();
    [
        ".mkv", ".mp4", ".avi", ".mov", ".wmv", ".ts", ".m2ts", ".flv", ".webm", ".m4v", ".mpg",
        ".mpeg",
    ]
    .iter()
    .any(|ext| low.ends_with(ext))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HistoryRecord {
    pub hash: String,
    pub title: String,
    pub magnet: String,
    pub last_opened: String,
    pub total_files: usize,
    pub watched_file_ids: Vec<i32>,
    pub favorite: bool,
}

pub fn load_history(path: &Path) -> Vec<HistoryRecord> {
    if !path.exists() {
        return Vec::new();
    }
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<HistoryRecord>>(&s).ok())
        .unwrap_or_default()
}

pub fn save_history(path: &Path, records: &[HistoryRecord]) {
    if let Ok(json) = serde_json::to_string_pretty(records) {
        let _ = fs::write(path, json);
    }
}

pub fn upsert_history_watch(
    records: &mut Vec<HistoryRecord>,
    hash: &str,
    title: &str,
    magnet: &str,
    total_files: usize,
    watched_file_id: Option<i32>,
) {
    let now = Local::now().format("%Y-%m-%d %H:%M").to_string();
    if let Some(pos) = records.iter().position(|r| r.hash == hash) {
        let mut rec = records.remove(pos);
        if !title.is_empty() {
            rec.title = title.to_string();
        }
        if !magnet.is_empty() {
            rec.magnet = magnet.to_string();
        }
        if total_files > 0 {
            rec.total_files = total_files;
        }
        rec.last_opened = now;
        if let Some(fid) = watched_file_id {
            if !rec.watched_file_ids.contains(&fid) {
                rec.watched_file_ids.push(fid);
            }
        }
        records.insert(0, rec);
    } else {
        let mut watched = Vec::new();
        if let Some(fid) = watched_file_id {
            watched.push(fid);
        }
        records.insert(
            0,
            HistoryRecord {
                hash: hash.to_string(),
                title: title.to_string(),
                magnet: magnet.to_string(),
                last_opened: now,
                total_files,
                watched_file_ids: watched,
                favorite: false,
            },
        );
    }
    if records.len() > 200 {
        records.truncate(200);
    }
}
