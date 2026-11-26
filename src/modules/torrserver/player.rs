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
    let mut found = Vec::new();
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".to_string());
    let pf86 = std::env::var("ProgramFiles(x86)")
        .unwrap_or_else(|_| "C:\\Program Files (x86)".to_string());
    let local_app = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    let user_profile = std::env::var("USERPROFILE").unwrap_or_default();
    let choco_bin = std::env::var("ChocolateyInstall")
        .map(|p| PathBuf::from(p).join("bin"))
        .unwrap_or_else(|_| PathBuf::from("C:\\ProgramData\\chocolatey\\bin"));

    let candidates: Vec<(&str, &str, Vec<&str>, Vec<PathBuf>)> = vec![
        (
            "potplayer",
            "PotPlayer",
            vec!["PotPlayerMini64.exe", "PotPlayerMini.exe", "PotPlayer64.exe", "PotPlayer.exe"],
            vec![
                PathBuf::from(&pf).join("DAUM\\PotPlayer\\PotPlayerMini64.exe"),
                PathBuf::from(&pf).join("DAUM\\PotPlayer\\PotPlayer64.exe"),
                PathBuf::from(&pf86).join("DAUM\\PotPlayer\\PotPlayerMini.exe"),
                PathBuf::from(&pf).join("PotPlayer\\PotPlayerMini64.exe"),
                PathBuf::from(&pf).join("PotPlayer\\PotPlayer64.exe"),
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
                PathBuf::from(&pf86).join("VideoLAN\\VLC\\vlc.exe"),
                PathBuf::from(&local_app).join("Programs\\VideoLAN\\VLC\\vlc.exe"),
                PathBuf::from(&local_app).join("Microsoft\\WindowsApps\\vlc.exe"),
                PathBuf::from(&user_profile).join("scoop\\apps\\vlc\\current\\vlc.exe"),
            ],
        ),
        (
            "mpc-be",
            "MPC-BE",
            vec!["mpc-be64.exe", "mpc-be.exe"],
            vec![
                PathBuf::from(&pf).join("MPC-BE x64\\mpc-be64.exe"),
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
    ];

    for (id, name, exe_names, paths) in candidates {
        let resolved = paths
            .into_iter()
            .find(|p| p.exists())
            .or_else(|| exe_names.iter().find_map(|exe| find_in_app_paths(exe)))
            .or_else(|| exe_names.iter().find_map(|exe| find_in_path(exe)));

        if let Some(p) = resolved {
            found.push(DetectedPlayer {
                id: id.to_string(),
                name: name.to_string(),
                exe_path: p,
            });
        }
    }

    found
}

fn find_in_app_paths(exe_name: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
            HKEY_LOCAL_MACHINE, KEY_READ, REG_EXPAND_SZ, REG_SZ,
        };

        let subkey_str = format!(
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{exe_name}\0"
        );
        let subkey: Vec<u16> = subkey_str.encode_utf16().collect();

        for root_key in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            unsafe {
                let mut hkey: HKEY = std::ptr::null_mut();
                if RegOpenKeyExW(root_key, subkey.as_ptr(), 0, KEY_READ, &mut hkey) == 0 {
                    let mut reg_type: u32 = 0;
                    let mut byte_len: u32 = 0;
                    if RegQueryValueExW(
                        hkey,
                        std::ptr::null(),
                        std::ptr::null(),
                        &mut reg_type,
                        std::ptr::null_mut(),
                        &mut byte_len,
                    ) == 0
                        && byte_len > 2
                        && (reg_type == REG_SZ || reg_type == REG_EXPAND_SZ)
                    {
                        let mut buf = vec![0u16; (byte_len as usize / 2) + 1];
                        if RegQueryValueExW(
                            hkey,
                            std::ptr::null(),
                            std::ptr::null(),
                            &mut reg_type,
                            buf.as_mut_ptr() as *mut u8,
                            &mut byte_len,
                        ) == 0
                        {
                            let raw = String::from_utf16_lossy(&buf)
                                .trim_end_matches('\0')
                                .trim()
                                .trim_matches('"')
                                .to_string();
                            if !raw.is_empty() {
                                let expanded = expand_env_vars(&raw);
                                let p = PathBuf::from(expanded);
                                if p.exists() {
                                    RegCloseKey(hkey);
                                    return Some(p);
                                }
                            }
                        }
                    }
                    RegCloseKey(hkey);
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
        if candidate.exists() {
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
