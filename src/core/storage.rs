use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

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
}

impl PortablePaths {
    pub fn init() -> Result<Self, String> {
        let exe_path = std::env::current_exe()
            .map_err(|e| format!("Не удалось определить путь к exe: {e}"))?;
        let exe_dir = exe_path
            .parent()
            .ok_or_else(|| "Не удалось определить директорию exe".to_string())?
            .to_path_buf();

        let data_dir = exe_dir.join("data");
        let torrserver_dir = data_dir.join("torrserver");
        let ts_cache_dir = torrserver_dir.join("ts_cache");
        let ts_playlists_dir = torrserver_dir.join("playlists");
        let ts_torrents_dir = torrserver_dir.join("torrents");
        let ts_posters_dir = torrserver_dir.join("posters");
        let modules_dir = data_dir.join("modules");

        for dir in [
            &data_dir,
            &torrserver_dir,
            &ts_cache_dir,
            &ts_playlists_dir,
            &ts_torrents_dir,
            &ts_posters_dir,
            &modules_dir,
        ] {
            fs::create_dir_all(dir)
                .map_err(|e| format!("Ошибка создания каталога {}: {e}", dir.display()))?;
        }

        Ok(Self {
            exe_path,
            exe_dir,
            config_path: data_dir.join("config.json"),
            torrserver_exe: torrserver_dir.join("TorrServer-windows-amd64.exe"),
            ts_history_path: torrserver_dir.join("history.json"),
            ts_torrents_meta_path: torrserver_dir.join("torrents_meta.json"),
            ts_torrents_links_path: torrserver_dir.join("torrents_links.json"),
            ts_torrents_saved_path: torrserver_dir.join("torrents_saved.json"),
            ts_torrents_dir,
            ts_posters_dir,
            ts_webview_dir: torrserver_dir.join("webview2"),
            data_dir,
            torrserver_dir,
            ts_cache_dir,
            ts_playlists_dir,
            modules_dir,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TorrentOriginMeta {
    pub magnet: String,
    pub tracker_url: String,
    pub tracker_name: String,
    pub rutracker_topic_id: String,
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
pub struct CoreConfig {
    pub theme_mode: i32, // 0 = System, 1 = Light, 2 = Dark
    pub autostart: bool,
    pub start_minimized: bool,
    pub keep_in_memory: bool,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            theme_mode: 0,
            autostart: false,
            start_minimized: false,
            keep_in_memory: true,
        }
    }
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
    pub torrents_sort_mode: i32, // 0 = Date desc, 1 = Date asc, 2 = Title, 3 = Size, 4 = Speed
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppConfig {
    pub core: CoreConfig,
    pub torrserver: TorrServerConfig,
}

pub fn load_config(paths: &PortablePaths) -> AppConfig {
    if !paths.config_path.exists() {
        let cfg = AppConfig::default();
        let _ = save_config(paths, &cfg);
        return cfg;
    }

    match fs::read_to_string(&paths.config_path) {
        Ok(raw) => serde_json::from_str::<AppConfig>(&raw).unwrap_or_default(),
        Err(_) => AppConfig::default(),
    }
}

pub fn save_config(paths: &PortablePaths, config: &AppConfig) -> Result<(), String> {
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Ошибка сериализации config.json: {e}"))?;
    let tmp_path = paths.config_path.with_extension("json.tmp");
    fs::write(&tmp_path, json)
        .map_err(|e| format!("Ошибка записи config.json.tmp: {e}"))?;
    if paths.config_path.exists() {
        let _ = fs::remove_file(&paths.config_path);
    }
    fs::rename(&tmp_path, &paths.config_path)
        .map_err(|e| format!("Ошибка сохранения config.json: {e}"))?;
    Ok(())
}
