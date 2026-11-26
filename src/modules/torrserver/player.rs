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

    let candidates: [(&str, &str, Vec<PathBuf>); 5] = [
        (
            "potplayer",
            "PotPlayer",
            vec![
                PathBuf::from(&pf).join("DAUM\\PotPlayer\\PotPlayerMini64.exe"),
                PathBuf::from(&pf86).join("DAUM\\PotPlayer\\PotPlayerMini.exe"),
                PathBuf::from("C:\\Program Files\\DAUM\\PotPlayer\\PotPlayerMini64.exe"),
            ],
        ),
        (
            "mpv",
            "MPV",
            vec![
                PathBuf::from(&pf).join("mpv\\mpv.exe"),
                PathBuf::from(&local_app).join("Programs\\mpv\\mpv.exe"),
                PathBuf::from(&pf).join("mpv.net\\mpvnet.exe"),
            ],
        ),
        (
            "vlc",
            "VLC Media Player",
            vec![
                PathBuf::from(&pf).join("VideoLAN\\VLC\\vlc.exe"),
                PathBuf::from(&pf86).join("VideoLAN\\VLC\\vlc.exe"),
            ],
        ),
        (
            "mpc-be",
            "MPC-BE",
            vec![
                PathBuf::from(&pf).join("MPC-BE x64\\mpc-be64.exe"),
                PathBuf::from(&pf86).join("MPC-BE\\mpc-be.exe"),
            ],
        ),
        (
            "mpc-hc",
            "MPC-HC",
            vec![
                PathBuf::from(&pf).join("MPC-HC\\mpc-hc64.exe"),
                PathBuf::from(&pf86).join("MPC-HC\\mpc-hc.exe"),
                PathBuf::from(&pf86).join("K-Lite Codec Pack\\MPC-HC64\\mpc-hc64.exe"),
            ],
        ),
    ];

    for (id, name, paths) in candidates {
        if let Some(p) = paths.into_iter().find(|p| p.exists()) {
            found.push(DetectedPlayer {
                id: id.to_string(),
                name: name.to_string(),
                exe_path: p,
            });
        } else if let Some(in_path) = find_in_path(match id {
            "potplayer" => "PotPlayerMini64.exe",
            "mpv" => "mpv.exe",
            "vlc" => "vlc.exe",
            "mpc-be" => "mpc-be64.exe",
            _ => "mpc-hc64.exe",
        }) {
            found.push(DetectedPlayer {
                id: id.to_string(),
                name: name.to_string(),
                exe_path: in_path,
            });
        }
    }

    found
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
