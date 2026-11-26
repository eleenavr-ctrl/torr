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
    pub poster: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TorrentBackupItem {
    pub hash: String,
    pub title: String,
    pub poster: String,
    pub data: String,
    pub magnet: String,
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

    pub fn add_backup_item(&self, item: &TorrentBackupItem) -> Result<ApiTorrent, String> {
        let link = if !item.magnet.trim().is_empty() {
            item.magnet.trim().to_string()
        } else if !item.hash.trim().is_empty() {
            format!("magnet:?xt=urn:btih:{}", item.hash.trim())
        } else {
            return Err("Элемент бэкапа не содержит magnet или hash".to_string());
        };

        let mut obj = serde_json::Map::new();
        obj.insert("action".to_string(), json!("add"));
        obj.insert("link".to_string(), json!(link));
        obj.insert("title".to_string(), json!(item.title));
        obj.insert("save_to_db".to_string(), json!(true));
        if !item.poster.trim().is_empty() {
            obj.insert("poster".to_string(), json!(item.poster));
        }
        if !item.data.trim().is_empty() {
            obj.insert("data".to_string(), json!(item.data));
        }

        let url = format!("{}/torrents", self.base_url);
        let resp = self
            .auth(self.client.post(&url))
            .json(&Value::Object(obj))
            .send()
            .map_err(|e| format!("Ошибка импорта раздачи: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {} при импорте раздачи", resp.status().as_u16()));
        }
        resp.json::<ApiTorrent>()
            .map_err(|e| format!("Ошибка ответа при импорте раздачи: {e}"))
    }

    pub fn export_torrents_json(&self) -> Result<(String, usize), String> {
        let list = self.list_torrents()?;
        let items: Vec<TorrentBackupItem> = list
            .into_iter()
            .filter(|t| !t.hash.trim().is_empty())
            .map(|t| TorrentBackupItem {
                magnet: format!("magnet:?xt=urn:btih:{}", t.hash.trim()),
                hash: t.hash,
                title: if !t.title.trim().is_empty() {
                    t.title
                } else {
                    t.name
                },
                poster: t.poster,
                data: t.data,
            })
            .collect();
        let count = items.len();
        let json = serde_json::to_string_pretty(&items)
            .map_err(|e| format!("Ошибка сериализации JSON: {e}"))?;
        Ok((json, count))
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

        // 1. Built-in TorrServer RuTor search (/search/?query=...)
        let mut rutor_note = String::new();
        let ts_search_url = format!("{}/search/?query={}", self.base_url, encoded);
        match self.auth(self.client.get(&ts_search_url)).send() {
            Ok(resp) if resp.status().is_success() => {
                let raw_text = resp.text().unwrap_or_default();
                let trimmed = raw_text.trim();
                if trimmed.is_empty() || trimmed == "null" || trimmed == "[]" {
                    let rutor_disabled = self
                        .get_settings()
                        .map(|s| !s.enable_rutor_search)
                        .unwrap_or(false);
                    rutor_note = if rutor_disabled {
                        "встроенный поиск RuTor отключён в настройках TorrServer".to_string()
                    } else {
                        "RuTor вернул 0 результатов".to_string()
                    };
                } else {
                    match serde_json::from_str::<Value>(trimmed) {
                        Ok(val) => {
                            let arr_opt = val.as_array().cloned().or_else(|| {
                                val.get("results")
                                    .or_else(|| val.get("data"))
                                    .and_then(|v| v.as_array())
                                    .cloned()
                            });
                            if let Some(items) = arr_opt {
                                let mapped: Vec<ApiSearchResult> =
                                    items.iter().filter_map(parse_rutor_value).collect();
                                if !mapped.is_empty() {
                                    diagnostics::info(
                                        "[TS:API]",
                                        format!(
                                            "Поиск TorrServer RuTor вернул {} результатов",
                                            mapped.len()
                                        ),
                                    );
                                    return Ok(mapped);
                                }
                                let rutor_disabled = self
                                    .get_settings()
                                    .map(|s| !s.enable_rutor_search)
                                    .unwrap_or(false);
                                rutor_note = if rutor_disabled {
                                    "встроенный поиск RuTor отключён в настройках TorrServer"
                                        .to_string()
                                } else {
                                    "RuTor вернул 0 подходящих раздач".to_string()
                                };
                            } else {
                                rutor_note = "RuTor вернул неожиданный формат JSON".to_string();
                            }
                        }
                        Err(e) => {
                            rutor_note = format!("RuTor JSON: {e}");
                        }
                    }
                }
            }
            Ok(resp) => {
                let code = resp.status().as_u16();
                let body = resp.text().unwrap_or_default();
                let body_trim = body.trim();
                let rutor_disabled = self
                    .get_settings()
                    .map(|s| !s.enable_rutor_search)
                    .unwrap_or(false);
                if rutor_disabled {
                    rutor_note =
                        format!("RuTor отключён в настройках TorrServer (HTTP {code})");
                } else if body_trim.is_empty() {
                    rutor_note = format!("RuTor HTTP {code}");
                } else {
                    rutor_note = format!("RuTor HTTP {code} ({body_trim})");
                }
            }
            Err(e) => {
                rutor_note = format!("RuTor недоступен: {e}");
            }
        }

        // 2. Fallback to public JacRed mirrors
        diagnostics::info(
            "[TS:API]",
            format!("{rutor_note} — выполняется fallback-поиск JacRed"),
        );

        let mirrors = [
            "https://jacred.xyz",
            "https://jacred.stream",
            "https://jr.maxvol.pro",
        ];
        let mut last_err = String::new();

        for base in mirrors {
            let jacred_url = format!("{base}/api/v1.0/torrents?search={encoded}");
            match self
                .client
                .get(&jacred_url)
                .header("User-Agent", "TorrServerUI/0.1.0")
                .send()
            {
                Ok(resp) if resp.status().is_success() => {
                    let raw_text = resp.text().unwrap_or_default();
                    let trimmed = raw_text.trim();
                    if trimmed.is_empty() || trimmed == "null" || trimmed == "[]" {
                        last_err = format!("{base}: пустой ответ");
                        continue;
                    }
                    match serde_json::from_str::<Vec<Value>>(trimmed) {
                        Ok(items) => {
                            let mapped: Vec<ApiSearchResult> =
                                items.iter().filter_map(parse_jacred_value).collect();
                            if !mapped.is_empty() {
                                diagnostics::info(
                                    "[TS:API]",
                                    format!(
                                        "JacRed ({base}) вернул {} результатов",
                                        mapped.len()
                                    ),
                                );
                                return Ok(mapped);
                            }
                            last_err = format!("{base}: 0 результатов");
                        }
                        Err(e) => {
                            last_err = format!("{base}: ошибка JSON ({e})");
                        }
                    }
                }
                Ok(resp) => {
                    last_err = format!("{base}: HTTP {}", resp.status().as_u16());
                }
                Err(e) => {
                    last_err = format!("{base}: {e}");
                }
            }
        }

        Err(format!(
            "Поиск не дал результатов ({rutor_note}; JacRed: {last_err}). Включите «Встроенный поиск RuTor» в Настройках и нажмите «Применить параметры движка (/settings)»."
        ))
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

fn first_non_empty_str(v: &Value, keys: &[&str]) -> String {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

fn first_i32(v: &Value, keys: &[&str]) -> i32 {
    for k in keys {
        if let Some(val) = v.get(*k) {
            if let Some(n) = val.as_i64() {
                return n as i32;
            }
            if let Some(s) = val.as_str() {
                if let Ok(n) = s.trim().parse::<i32>() {
                    return n;
                }
            }
        }
    }
    0
}

fn parse_rutor_value(v: &Value) -> Option<ApiSearchResult> {
    let title = first_non_empty_str(v, &["Title", "title", "Name", "name"]);
    let magnet_direct = first_non_empty_str(v, &["Magnet", "magnet"]);
    let link = first_non_empty_str(v, &["Link", "link"]);
    let hash = first_non_empty_str(v, &["Hash", "hash"]);

    let magnet = if !magnet_direct.is_empty() {
        magnet_direct
    } else if link.starts_with("magnet:") {
        link
    } else if !hash.is_empty() {
        format!("magnet:?xt=urn:btih:{hash}")
    } else if !link.is_empty() {
        link
    } else {
        return None;
    };

    let (size_text, size_bytes) = match v.get("Size").or_else(|| v.get("size")) {
        Some(Value::Number(n)) => {
            let b = n.as_u64().unwrap_or(0);
            (format_bytes(b), b as f64)
        }
        Some(Value::String(s)) => {
            let t = s.trim().to_string();
            let b = parse_size_bytes(&t);
            (if t.is_empty() { "—".to_string() } else { t }, b)
        }
        _ => ("—".to_string(), 0.0),
    };

    let seeds = first_i32(v, &["Seed", "Seeds", "seed", "seeds", "sid"]);
    let peers = first_i32(v, &["Peer", "Peers", "peer", "peers", "pir"]);
    let tracker = {
        let t = first_non_empty_str(v, &["Tracker", "tracker"]);
        if t.is_empty() {
            "RuTor".to_string()
        } else {
            t
        }
    };
    let disp_title = if title.is_empty() {
        magnet.clone()
    } else {
        title
    };
    let quality = detect_quality(&disp_title);

    Some(ApiSearchResult {
        title: disp_title,
        magnet,
        size_text,
        size_bytes,
        seeds,
        peers,
        quality,
        tracker,
    })
}

fn parse_jacred_value(v: &Value) -> Option<ApiSearchResult> {
    let magnet = first_non_empty_str(v, &["magnet", "Magnet"]);
    if magnet.is_empty() {
        return None;
    }
    let title = first_non_empty_str(v, &["title", "Title", "name", "Name"]);
    let size_u64 = v
        .get("size")
        .or_else(|| v.get("Size"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let seeds = first_i32(v, &["sid", "Seed", "seeds"]);
    let peers = first_i32(v, &["pir", "Peer", "peers"]);
    let tracker = {
        let t = first_non_empty_str(v, &["tracker", "Tracker"]);
        if t.is_empty() {
            "JacRed".to_string()
        } else {
            t
        }
    };
    let disp_title = if title.is_empty() {
        magnet.clone()
    } else {
        title
    };
    let quality = detect_quality(&disp_title);

    Some(ApiSearchResult {
        title: disp_title,
        magnet,
        size_text: format_bytes(size_u64),
        size_bytes: size_u64 as f64,
        seeds,
        peers,
        quality,
        tracker,
    })
}

fn parse_size_bytes(size_str: &str) -> f64 {
    let normalized = size_str.replace(',', ".").to_lowercase();
    let num_str: String = normalized
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let num: f64 = num_str.parse::<f64>().unwrap_or(0.0);
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

pub fn parse_backup_json(raw: &str) -> Result<Vec<TorrentBackupItem>, String> {
    let cleaned = raw.trim_start_matches('\u{feff}').trim();
    if cleaned.is_empty() {
        return Err("Файл импорта пуст".to_string());
    }

    let val: Value =
        serde_json::from_str(cleaned).map_err(|e| format!("Некорректный JSON-файл: {e}"))?;

    let items_slice: Vec<Value> = if let Some(arr) = val.as_array() {
        arr.clone()
    } else if let Some(arr) = val
        .get("torrents")
        .or_else(|| val.get("Torrents"))
        .or_else(|| val.get("items"))
        .and_then(|v| v.as_array())
    {
        arr.clone()
    } else if val.is_object() {
        vec![val]
    } else {
        return Err("Ожидался JSON-массив раздач".to_string());
    };

    let mut out = Vec::new();
    for item in items_slice {
        match item {
            Value::String(s) => {
                let link = s.trim().to_string();
                if !link.is_empty() {
                    out.push(TorrentBackupItem {
                        magnet: link,
                        ..Default::default()
                    });
                }
            }
            Value::Object(_) => {
                let hash = first_non_empty_str(&item, &["hash", "Hash"]);
                let magnet = first_non_empty_str(&item, &["magnet", "Magnet", "link", "Link"]);
                if hash.is_empty() && magnet.is_empty() {
                    continue;
                }
                let title = first_non_empty_str(&item, &["title", "Title", "name", "Name"]);
                let poster = first_non_empty_str(&item, &["poster", "Poster"]);
                let data = first_non_empty_str(&item, &["data", "Data"]);
                out.push(TorrentBackupItem {
                    hash,
                    title,
                    poster,
                    data,
                    magnet,
                });
            }
            _ => {}
        }
    }

    if out.is_empty() {
        return Err("В JSON-файле не найдено ни одной раздачи (hash/magnet)".to_string());
    }
    Ok(out)
}
