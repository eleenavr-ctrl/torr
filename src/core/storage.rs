pub use crate::core::config::{AppConfig, CoreConfig};
use crate::core::config::{load_settings, save_settings};
use crate::core::paths::AppPaths;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Backwards-compatible portable paths facade backed by `AppPaths` (`UserData/`).
#[derive(Debug, Clone)]
pub struct PortablePaths {
    pub exe_path: PathBuf,
    pub exe_dir: PathBuf,
    pub data_dir: PathBuf,
    pub config_path: PathBuf,
    pub torrserver_dir: PathBuf,
    pub torrserver_exe: PathBuf,
    pub ts_cache_dir: PathBuf,
    pub ts_playlists_dir: PathBuf,
    pub ts_history_path: PathBuf,
    pub ts_torrents_meta_path: PathBuf,
    pub ts_torrents_links_path: PathBuf,
    pub ts_torrents_saved_path: PathBuf,
    pub ts_torrents_dir: PathBuf,
    pub ts_posters_dir: PathBuf,
    pub ts_webview_dir: PathBuf,
    pub modules_dir: PathBuf,
    pub app_paths: AppPaths,
}

impl From<AppPaths> for PortablePaths {
    fn from(ap: AppPaths) -> Self {
        Self {
            exe_path: ap.exe_path.clone(),
            exe_dir: ap.exe_dir.clone(),
            data_dir: ap.user_data_dir.clone(),
            config_path: ap.settings_path.clone(),
            torrserver_dir: ap.torrserver_dir.clone(),
            torrserver_exe: ap.torrserver_exe.clone(),
            ts_cache_dir: ap.ts_cache_dir.clone(),
            ts_playlists_dir: ap.ts_playlists_dir.clone(),
            ts_history_path: ap.ts_history_path.clone(),
            ts_torrents_meta_path: ap.ts_torrents_meta_path.clone(),
            ts_torrents_links_path: ap.ts_torrents_links_path.clone(),
            ts_torrents_saved_path: ap.ts_torrents_saved_path.clone(),
            ts_torrents_dir: ap.ts_torrents_dir.clone(),
            ts_posters_dir: ap.ts_posters_dir.clone(),
            ts_webview_dir: ap.ts_webview_dir.clone(),
            modules_dir: ap.modules_dir.clone(),
            app_paths: ap,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TorrentOriginMeta {
    pub magnet: String,
    pub tracker_url: String,
    pub tracker_name: String,
    pub rutracker_topic_id: String,
    pub source_title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SavedTorrentTask {
    pub hash: String,
    pub title: String,
    pub magnet: String,
    pub torrent_file: String,
    pub poster: String,
    pub data: String,
    pub added_ts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TorrServerConfig {
    pub mode: i32, // 0 = Local, 1 = Remote
    pub port: u16,
    pub remote_url: String,
    pub username: String,
    pub password: String,
    pub preferred_engine_tag: String,
    pub engine_watchdog: bool,
    pub keep_engine_on_exit: bool,
    pub torrents_sort_mode: i32,
    pub player_id: String,
    pub player_custom_path: String,
    pub player_args: String,
    pub clipboard_watch: bool,
    pub only_video_files: bool,
    pub cache_size_mb: i64,
    pub preload_percent: i64,
    pub read_ahead_percent: i64,
    pub connections_limit: i64,
    pub use_disk: bool,
    pub enable_ipv6: bool,
    pub enable_dlna: bool,
    pub enable_rutor: bool,
    pub rutracker_enabled: bool,
    pub rutracker_mirror: String,
    pub rutracker_username: String,
    pub rutracker_password: String,
    pub rutracker_proxy: String,
    pub rutracker_cookie: String,
    pub rutracker_user_agent: String,
    pub rutracker_use_webview: bool,
    pub kinopoisk_api_key: String,
}

impl Default for TorrServerConfig {
    fn default() -> Self {
        Self {
            mode: 0,
            port: 8090,
            remote_url: "http://127.0.0.1:8090".to_string(),
            username: String::new(),
            password: String::new(),
            preferred_engine_tag: String::new(),
            engine_watchdog: true,
            keep_engine_on_exit: false,
            torrents_sort_mode: 0,
            player_id: "auto".to_string(),
            player_custom_path: String::new(),
            player_args: String::new(),
            clipboard_watch: false,
            only_video_files: true,
            cache_size_mb: 64,
            preload_percent: 50,
            read_ahead_percent: 95,
            connections_limit: 25,
            use_disk: false,
            enable_ipv6: false,
            enable_dlna: false,
            enable_rutor: true,
            rutracker_enabled: false,
            rutracker_mirror: "https://rutracker.org".to_string(),
            rutracker_username: String::new(),
            rutracker_password: String::new(),
            rutracker_proxy: String::new(),
            rutracker_cookie: String::new(),
            rutracker_user_agent: String::new(),
            rutracker_use_webview: false,
            kinopoisk_api_key: String::new(),
        }
    }
}

impl TorrServerConfig {
    pub fn effective_base_url(&self) -> String {
        if self.mode == 1 {
            let trimmed = self.remote_url.trim().trim_end_matches('/');
            if trimmed.is_empty() {
                format!("http://127.0.0.1:{}", self.port)
            } else if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                trimmed.to_string()
            } else {
                format!("http://{trimmed}")
            }
        } else {
            format!("http://127.0.0.1:{}", self.port)
        }
    }

    pub fn effective_rutracker_mirror(&self) -> String {
        let mut trimmed = self.rutracker_mirror.trim().trim_end_matches('/').to_string();
        if let Some(stripped) = trimmed.strip_suffix("/forum") {
            trimmed = stripped.to_string();
        }
        if trimmed.is_empty() {
            "https://rutracker.org".to_string()
        } else if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            trimmed
        } else {
            format!("https://{trimmed}")
        }
    }

    pub fn has_rutracker_credentials(&self) -> bool {
        self.rutracker_use_webview
            || !self.rutracker_cookie.trim().is_empty()
            || (!self.rutracker_username.trim().is_empty() && !self.rutracker_password.is_empty())
    }

    pub fn effective_rutracker_user_agent(&self) -> String {
        let mut cleaned = self
            .rutracker_user_agent
            .replace(['\r', '\n'], " ")
            .trim()
            .to_string();
        if let Some(rest) = cleaned
            .strip_prefix("User-Agent:")
            .or_else(|| cleaned.strip_prefix("user-agent:"))
        {
            cleaned = rest.trim().to_string();
        }
        if cleaned.is_empty() {
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36".to_string()
        } else {
            cleaned
        }
    }
}

pub fn load_config(paths: &PortablePaths) -> AppConfig {
    load_settings(&paths.app_paths).unwrap_or_default()
}

pub fn save_config(paths: &PortablePaths, config: &AppConfig) -> Result<(), String> {
    save_settings(&paths.app_paths, config).map_err(|e| e.to_string())
}
