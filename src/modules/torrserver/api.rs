use crate::core::diagnostics;
use crate::core::storage::TorrServerConfig;
use reqwest::blocking::multipart::{Form, Part};
use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ApiFileStat {
    #[serde(alias = "Id")]
    pub id: i32,
    #[serde(alias = "Path")]
    pub path: String,
    #[serde(alias = "Length")]
    pub length: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ApiTorrent {
    pub hash: String,
    pub title: String,
    pub name: String,
    pub torrent_size: u64,
    pub loaded_size: u64,
    pub download_speed: f64,
    pub upload_speed: f64,
    pub total_peers: i32,
    pub connected_seeders: i32,
    pub active_peers: i32,
    pub stat: i32,
    pub stat_string: String,
    pub file_stats: Vec<ApiFileStat>,
    pub data: String,
}

impl ApiTorrent {
    pub fn display_title(&self) -> String {
        if !self.title.trim().is_empty() {
            self.title.clone()
        } else if !self.name.trim().is_empty() {
            self.name.clone()
        } else {
            self.hash.clone()
        }
    }

    pub fn files(&self) -> Vec<ApiFileStat> {
        if !self.file_stats.is_empty() {
            return self.file_stats.clone();
        }
        if !self.data.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(&self.data) {
                if let Some(arr) = v
                    .get("TorrServer")
                    .and_then(|t| t.get("Files"))
                    .and_then(|f| f.as_array())
                {
                    let mut out = Vec::new();
                    for item in arr {
                        let id = item.get("Id").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
                        let path = item
                            .get("Path")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let length = item.get("Length").and_then(|x| x.as_u64()).unwrap_or(0);
                        out.push(ApiFileStat { id, path, length });
                    }
                    return out;
                }
            }
        }
        Vec::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TorrServerEngineSettings {
    #[serde(rename = "CacheSize")]
    pub cache_size: i64,
    #[serde(rename = "PreloadCache")]
    pub preload_cache: i64,
    #[serde(rename = "ReaderReadAHead")]
    pub reader_read_ahead: i64,
    #[serde(rename = "ConnectionsLimit")]
    pub connections_limit: i64,
    #[serde(rename = "EnableIPv6")]
    pub enable_ipv6: bool,
    #[serde(rename = "UseDisk")]
    pub use_disk: bool,
    #[serde(rename = "EnableDLNA")]
    pub enable_dlna: bool,
    #[serde(rename = "EnableRutorSearch")]
    pub enable_rutor_search: bool,
}

#[derive(Debug, Clone)]
pub struct ApiSearchResult {
    pub title: String,
    pub magnet: String,
    pub size_text: String,
    pub size_bytes: f64,
    pub seeds: i32,
    pub peers: i32,
    pub quality: String,
    pub tracker: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawRutorResult {
    #[serde(alias = "Title")]
    title: String,
    #[serde(alias = "Magnet")]
    magnet: String,
    #[serde(alias = "Size")]
    size: String,
    #[serde(alias = "Seed")]
    seed: i32,
    #[serde(alias = "Peer")]
    peer: i32,
    #[serde(alias = "Tracker")]
    tracker: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawJacRedResult {
    title: String,
    magnet: String,
    size: u64,
    sid: i32,
    pir: i32,
    tracker: String,
}

pub struct TorrServerApi {
    base_url: String,
    username: String,
    password: String,
    client: Client,
}

impl TorrServerApi {
    pub fn from_config(cfg: &TorrServerConfig) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(7))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            base_url: cfg.effective_base_url(),
            username: cfg.username.clone(),
            password: cfg.password.clone(),
            client,
        }
    }

    fn auth(&self, req: RequestBuilder) -> RequestBuilder {
        if !self.username.is_empty() || !self.password.is_empty() {
            req.basic_auth(&self.username, Some(&self.password))
        } else {
            req
        }
    }

    pub fn echo(&self) -> Result<(String, u128), String> {
        let url = format!("{}/echo", self.base_url);
        let start = Instant::now();
        let resp = self
            .auth(self.client.get(&url))
            .send()
            .map_err(|e| format!("Ошибка подключения к /echo: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {} от /echo", resp.status().as_u16()));
        }
        let ping_ms = start.elapsed().as_millis();
        let ver = resp.text().unwrap_or_default().trim().to_string();
        Ok((ver, ping_ms))
    }

    pub fn list_torrents(&self) -> Result<Vec<ApiTorrent>, String> {
        let url = format!("{}/torrents", self.base_url);
        let resp = self
            .auth(self.client.post(&url))
            .json(&json!({ "action": "list" }))
            .send()
            .map_err(|e| format!("Ошибка запроса /torrents list: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {} от /torrents", resp.status().as_u16()));
        }
        let text = resp.text().unwrap_or_else(|_| "[]".to_string());
        if text.trim().is_empty() || text.trim() == "null" {
            return Ok(Vec::new());
        }
        serde_json::from_str::<Vec<ApiTorrent>>(&text)
            .map_err(|e| format!("Ошибка разбора списка раздач: {e}"))
    }

    pub fn add_magnet(&self, link: &str, title: Option<&str>) -> Result<ApiTorrent, String> {
        let url = format!("{}/torrents", self.base_url);
        let body = json!({
            "action": "add",
            "link": link,
            "title": title.unwrap_or(""),
            "save_to_db": true
        });
        let resp = self
            .auth(self.client.post(&url))
            .json(&body)
            .send()
            .map_err(|e| format!("Ошибка добавления magnet: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {} при добавлении magnet", resp.status().as_u16()));
        }
        resp.json::<ApiTorrent>()
            .map_err(|e| format!("Ошибка ответа при добавлении magnet: {e}"))
    }

    pub fn get_torrent(&self, hash: &str) -> Result<ApiTorrent, String> {
        let url = format!("{}/torrents", self.base_url);
        let resp = self
            .auth(self.client.post(&url))
            .json(&json!({ "action": "get", "hash": hash }))
            .send()
            .map_err(|e| format!("Ошибка /torrents get: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {} от /torrents get", resp.status().as_u16()));
        }
        resp.json::<ApiTorrent>()
            .map_err(|e| format!("Ошибка разбора торрента: {e}"))
    }

    pub fn drop_torrent(&self, hash: &str) -> Result<(), String> {
        let url = format!("{}/torrents", self.base_url);
        let resp = self
            .auth(self.client.post(&url))
            .json(&json!({ "action": "drop", "hash": hash }))
            .send()
            .map_err(|e| format!("Ошибка /torrents drop: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {} при сбросе кеша", resp.status().as_u16()));
        }
        Ok(())
    }

    pub fn remove_torrent(&self, hash: &str) -> Result<(), String> {
        let url = format!("{}/torrents", self.base_url);
        let resp = self
            .auth(self.client.post(&url))
            .json(&json!({ "action": "rem", "hash": hash }))
            .send()
            .map_err(|e| format!("Ошибка /torrents rem: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {} при удалении раздачи", resp.status().as_u16()));
        }
        Ok(())
    }

    pub fn upload_torrent_file(&self, path: &Path) -> Result<ApiTorrent, String> {
        let bytes = std::fs::read(path)
            .map_err(|e| format!("Не удалось прочитать .torrent файл: {e}"))?;
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("upload.torrent")
            .to_string();

        let part = Part::bytes(bytes).file_name(filename);
        let form = Form::new().part("file", part);
        let url = format!("{}/torrent/upload", self.base_url);
        let resp = self
            .auth(self.client.post(&url))
            .multipart(form)
            .send()
            .map_err(|e| format!("Ошибка загрузки .torrent на сервер: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("HTTP {} при загрузке .torrent", resp.status().as_u16()));
        }
        resp.json::<ApiTorrent>()
            .map_err(|e| format!("Ошибка ответа /torrent/upload: {e}"))
    }

    pub fn get_settings(&self) -> Result<TorrServerEngineSettings, String> {
        let url = format!("{}/settings", self.base_url);
        let resp = self
            .auth(self.client.post(&url))
            .json(&json!({ "action": "get" }))
            .send()
            .map_err(|e| format!("Ошибка /settings get: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {} от /settings get", resp.status().as_u16()));
        }
        resp.json::<TorrServerEngineSettings>()
            .map_err(|e| format!("Ошибка разбора настроек движка: {e}"))
    }

    pub fn set_settings(&self, sets: &TorrServerEngineSettings) -> Result<(), String> {
        let url = format!("{}/settings", self.base_url);
        // Fetch current object so unrelated TorrServer fields are preserved
        let mut merged = match self
            .auth(self.client.post(&url))
            .json(&json!({ "action": "get" }))
            .send()
        {
            Ok(resp) if resp.status().is_success() => {
                resp.json::<Value>().unwrap_or_else(|_| json!({}))
            }
            _ => json!({}),
        };

        if let Some(obj) = merged.as_object_mut() {
            obj.insert("CacheSize".to_string(), json!(sets.cache_size));
            obj.insert("PreloadCache".to_string(), json!(sets.preload_cache));
            obj.insert("ReaderReadAHead".to_string(), json!(sets.reader_read_ahead));
            obj.insert("ConnectionsLimit".to_string(), json!(sets.connections_limit));
            obj.insert("EnableIPv6".to_string(), json!(sets.enable_ipv6));
            obj.insert("UseDisk".to_string(), json!(sets.use_disk));
            obj.insert("EnableDLNA".to_string(), json!(sets.enable_dlna));
            obj.insert("EnableRutorSearch".to_string(), json!(sets.enable_rutor_search));
        }

        let resp = self
            .auth(self.client.post(&url))
            .json(&json!({ "action": "set", "sets": merged }))
            .send()
            .map_err(|e| format!("Ошибка /settings set: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {} при сохранении /settings", resp.status().as_u16()));
        }
        Ok(())
    }

    pub fn shutdown_server(&self) -> Result<(), String> {
        let short_client = Client::builder()
            .timeout(Duration::from_millis(800))
            .build()
            .unwrap_or_else(|_| Client::new());
        let url = format!("{}/shutdown", self.base_url);
        let mut req = short_client.post(&url);
        if !self.username.is_empty() || !self.password.is_empty() {
            req = req.basic_auth(&self.username, Some(&self.password));
        }
        let _ = req.send();
        Ok(())
    }

    pub fn search(&self, query: &str) -> Result<Vec<ApiSearchResult>, String> {
        let encoded = urlencoding::encode(query.trim());

        // 1. Built-in TorrServer RuTor search
        let ts_search_url = format!("{}/search?query={}", self.base_url, encoded);
        if let Ok(resp) = self.auth(self.client.get(&ts_search_url)).send() {
            if resp.status().is_success() {
                if let Ok(items) = resp.json::<Vec<RawRutorResult>>() {
                    if !items.is_empty() {
                        diagnostics::info(
                            "[TS:API]",
                            format!("Поиск TorrServer RuTor вернул {} результатов", items.len()),
                        );
                        return Ok(items
                            .into_iter()
                            .filter(|i| !i.magnet.is_empty())
                            .map(|i| {
                                let quality = detect_quality(&i.title);
                                let size_bytes = parse_size_bytes(&i.size);
                                ApiSearchResult {
                                    title: i.title,
                                    magnet: i.magnet,
                                    size_text: if i.size.is_empty() { "—".into() } else { i.size },
                                    size_bytes,
                                    seeds: i.seed,
                                    peers: i.peer,
                                    quality,
                                    tracker: if i.tracker.is_empty() {
                                        "RuTor".to_string()
                                    } else {
                                        i.tracker
                                    },
                                }
                            })
                            .collect());
                    }
                }
            }
        }

        // 2. Fallback to public JacRed API
        diagnostics::info("[TS:API]", "RuTor пуст или отключён — выполняется fallback-поиск JacRed");
        let jacred_url = format!("https://jacred.xyz/api/v1.0/torrents?search={}", encoded);
        let resp = self
            .client
            .get(&jacred_url)
            .header("User-Agent", "TorrServerUI/0.1.0")
            .send()
            .map_err(|e| format!("Ошибка поиска (RuTor и JacRed недоступны): {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("JacRed вернул HTTP {}", resp.status().as_u16()));
        }

        let items = resp
            .json::<Vec<RawJacRedResult>>()
            .map_err(|e| format!("Ошибка разбора ответа JacRed: {e}"))?;

        Ok(items
            .into_iter()
            .filter(|i| !i.magnet.is_empty())
            .map(|i| {
                let quality = detect_quality(&i.title);
                ApiSearchResult {
                    title: i.title,
                    magnet: i.magnet,
                    size_text: format_bytes(i.size),
                    size_bytes: i.size as f64,
                    seeds: i.sid,
                    peers: i.pir,
                    quality,
                    tracker: if i.tracker.is_empty() {
                        "JacRed".to_string()
                    } else {
                        i.tracker
                    },
                }
            })
            .collect())
    }

    pub fn stream_url(&self, hash: &str, file_id: i32, file_name: &str) -> String {
        let encoded_name = urlencoding::encode(file_name);
        let base_with_auth = if !self.username.is_empty() || !self.password.is_empty() {
            if let Some(rest) = self.base_url.strip_prefix("http://") {
                format!(
                    "http://{}:{}@{}",
                    urlencoding::encode(&self.username),
                    urlencoding::encode(&self.password),
                    rest
                )
            } else if let Some(rest) = self.base_url.strip_prefix("https://") {
                format!(
                    "https://{}:{}@{}",
                    urlencoding::encode(&self.username),
                    urlencoding::encode(&self.password),
                    rest
                )
            } else {
                self.base_url.clone()
            }
        } else {
            self.base_url.clone()
        };

        format!(
            "{}/stream/{}?link={}&index={}&play",
            base_with_auth, encoded_name, hash, file_id
        )
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} ГБ", b / GB)
    } else if b >= MB {
        format!("{:.1} МБ", b / MB)
    } else if b >= KB {
        format!("{:.0} КБ", b / KB)
    } else {
        format!("{bytes} Б")
    }
}

pub fn format_speed(bytes_per_sec: f64) -> String {
    if bytes_per_sec >= 1024.0 * 1024.0 {
        format!("{:.2} МБ/с", bytes_per_sec / (1024.0 * 1024.0))
    } else if bytes_per_sec >= 1024.0 {
        format!("{:.0} КБ/с", bytes_per_sec / 1024.0)
    } else {
        "0 КБ/с".to_string()
    }
}

fn detect_quality(title: &str) -> String {
    let low = title.to_lowercase();
    let mut tags = Vec::new();
    if low.contains("2160p") || low.contains("4k") || low.contains("uhd") {
        tags.push("4K");
    } else if low.contains("1080p") || low.contains("fhd") {
        tags.push("1080p");
    } else if low.contains("720p") {
        tags.push("720p");
    }
    if low.contains("hdr") || low.contains("dolby vision") || low.contains("dovi") {
        tags.push("HDR");
    }
    tags.join(" ")
}

fn parse_size_bytes(size_str: &str) -> f64 {
    let normalized = size_str.replace(',', ".").to_lowercase();
    let num: f64 = normalized
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    if normalized.contains("gb") || normalized.contains("гб") {
        num * 1024.0 * 1024.0 * 1024.0
    } else if normalized.contains("mb") || normalized.contains("мб") {
        num * 1024.0 * 1024.0
    } else if normalized.contains("kb") || normalized.contains("кб") {
        num * 1024.0
    } else {
        num
    }
}
