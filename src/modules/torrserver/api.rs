use crate::core::diagnostics;
use crate::core::storage::TorrServerConfig;
use chrono::{Datelike, DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use reqwest::blocking::multipart::{Form, Part};
use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    T: Default + Deserialize<'de>,
    D: Deserializer<'de>,
{
    let opt = Option::<T>::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ApiFileStat {
    #[serde(alias = "Id", deserialize_with = "null_default")]
    pub id: i32,
    #[serde(alias = "Path", deserialize_with = "null_default")]
    pub path: String,
    #[serde(alias = "Length", deserialize_with = "null_default")]
    pub length: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ApiTorrent {
    #[serde(deserialize_with = "null_default")]
    pub hash: String,
    #[serde(deserialize_with = "null_default")]
    pub title: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "null_default")]
    pub category: String,
    #[serde(deserialize_with = "null_default")]
    pub timestamp: i64,
    #[serde(deserialize_with = "null_default")]
    pub torrent_size: u64,
    #[serde(deserialize_with = "null_default")]
    pub loaded_size: u64,
    #[serde(deserialize_with = "null_default")]
    pub download_speed: f64,
    #[serde(deserialize_with = "null_default")]
    pub upload_speed: f64,
    #[serde(deserialize_with = "null_default")]
    pub total_peers: i32,
    #[serde(deserialize_with = "null_default")]
    pub connected_seeders: i32,
    #[serde(deserialize_with = "null_default")]
    pub active_peers: i32,
    #[serde(deserialize_with = "null_default")]
    pub stat: i32,
    #[serde(deserialize_with = "null_default")]
    pub stat_string: String,
    #[serde(deserialize_with = "null_default")]
    pub poster: String,
    #[serde(deserialize_with = "null_default")]
    pub file_stats: Vec<ApiFileStat>,
    #[serde(deserialize_with = "null_default")]
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
    #[serde(rename = "CacheSize", deserialize_with = "null_default")]
    pub cache_size: i64,
    #[serde(rename = "PreloadCache", deserialize_with = "null_default")]
    pub preload_cache: i64,
    #[serde(rename = "ReaderReadAHead", deserialize_with = "null_default")]
    pub reader_read_ahead: i64,
    #[serde(rename = "ConnectionsLimit", deserialize_with = "null_default")]
    pub connections_limit: i64,
    #[serde(rename = "EnableIPv6", deserialize_with = "null_default")]
    pub enable_ipv6: bool,
    #[serde(rename = "UseDisk", deserialize_with = "null_default")]
    pub use_disk: bool,
    #[serde(rename = "EnableDLNA", deserialize_with = "null_default")]
    pub enable_dlna: bool,
    #[serde(rename = "EnableRutorSearch", deserialize_with = "null_default")]
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
    pub date_ts: i64,
    pub date_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TorrentBackupItem {
    #[serde(deserialize_with = "null_default")]
    pub hash: String,
    #[serde(deserialize_with = "null_default")]
    pub title: String,
    #[serde(deserialize_with = "null_default")]
    pub poster: String,
    #[serde(deserialize_with = "null_default")]
    pub data: String,
    #[serde(deserialize_with = "null_default")]
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
        self.upload_torrent_bytes(bytes, filename)
    }

    pub fn upload_torrent_bytes(
        &self,
        bytes: Vec<u8>,
        filename: String,
    ) -> Result<ApiTorrent, String> {
        let part = Part::bytes(bytes).file_name(filename);
        let form = Form::new().text("save", "true").part("file", part);
        let url = format!("{}/torrent/upload?save=true", self.base_url);
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

    pub fn search_rutor(&self, query: &str) -> Result<Vec<ApiSearchResult>, String> {
        let encoded = urlencoding::encode(query.trim());
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
                    if rutor_disabled {
                        Err("встроенный поиск RuTor отключён в настройках TorrServer".to_string())
                    } else {
                        Ok(Vec::new())
                    }
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
                                }
                                Ok(mapped)
                            } else {
                                Err("RuTor вернул неожиданный формат JSON".to_string())
                            }
                        }
                        Err(e) => Err(format!("RuTor JSON: {e}")),
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
                    Err(format!(
                        "RuTor отключён в настройках TorrServer (HTTP {code})"
                    ))
                } else if body_trim.is_empty() {
                    Err(format!("RuTor HTTP {code}"))
                } else {
                    Err(format!("RuTor HTTP {code} ({body_trim})"))
                }
            }
            Err(e) => Err(format!("RuTor недоступен: {e}")),
        }
    }

    pub fn search_jacred(&self, query: &str) -> Result<Vec<ApiSearchResult>, String> {
        let encoded = urlencoding::encode(query.trim());
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

        Err(last_err)
    }

    pub fn fetch_top_24h_jacred(&self) -> Result<Vec<ApiSearchResult>, String> {
        let mirrors = [
            "https://jacred.xyz",
            "https://jacred.stream",
            "https://jr.maxvol.pro",
        ];
        let cur_year = Utc::now().year();
        let query_paths = [
            "/api/v1.0/torrents?sort=date".to_string(),
            format!("/api/v1.0/torrents?search={cur_year}&sort=sid"),
        ];
        let mut last_err = String::new();

        for base in mirrors {
            for rel in &query_paths {
                let url = format!("{base}{rel}");
                match self
                    .client
                    .get(&url)
                    .header("User-Agent", "TorrServerUI/0.1.0")
                    .send()
                {
                    Ok(resp) if resp.status().is_success() => {
                        let raw_text = resp.text().unwrap_or_default();
                        let trimmed = raw_text.trim();
                        if trimmed.is_empty() || trimmed == "null" || trimmed == "[]" {
                            last_err = format!("{base}: пустой список");
                            continue;
                        }
                        if let Ok(items) = serde_json::from_str::<Vec<Value>>(trimmed) {
                            let mut parsed: Vec<ApiSearchResult> =
                                items.iter().filter_map(parse_jacred_value).collect();
                            if !parsed.is_empty() {
                                let now_ts = Utc::now().timestamp();
                                let cutoff_24h = now_ts - 86_400;
                                let cutoff_72h = now_ts - 86_400 * 3;
                                let recent_24: Vec<ApiSearchResult> = parsed
                                    .iter()
                                    .filter(|r| r.date_ts >= cutoff_24h)
                                    .cloned()
                                    .collect();
                                if recent_24.len() >= 8 {
                                    parsed = recent_24;
                                } else {
                                    let recent_72: Vec<ApiSearchResult> = parsed
                                        .iter()
                                        .filter(|r| r.date_ts >= cutoff_72h)
                                        .cloned()
                                        .collect();
                                    if recent_72.len() >= 8 {
                                        parsed = recent_72;
                                    }
                                }
                                parsed.sort_by(|a, b| b.seeds.cmp(&a.seeds));
                                diagnostics::info(
                                    "[TS:API]",
                                    format!(
                                        "Топ 24ч JacRed ({base}) вернул {} результатов",
                                        parsed.len()
                                    ),
                                );
                                return Ok(parsed);
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
        }

        Err(if last_err.is_empty() {
            "JacRed не вернул популярные раздачи".to_string()
        } else {
            last_err
        })
    }

    pub fn fetch_top_24h_rutor_direct(&self) -> Result<Vec<ApiSearchResult>, String> {
        let urls = [
            "https://rutor.info/top",
            "https://rutor.is/top",
            "http://rutor.info/top",
        ];
        let short_client = Client::builder()
            .timeout(Duration::from_secs(6))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
            .build()
            .unwrap_or_else(|_| self.client.clone());

        let mut last_err = String::new();
        for url in urls {
            match short_client.get(url).send() {
                Ok(resp) if resp.status().is_success() => {
                    let html = resp.text().unwrap_or_default();
                    let parsed = parse_rutor_top_html(&html);
                    if !parsed.is_empty() {
                        diagnostics::info(
                            "[TS:API]",
                            format!(
                                "Топ RuTor ({url}) вернул {} популярных раздач",
                                parsed.len()
                            ),
                        );
                        return Ok(parsed);
                    }
                    last_err = format!("{url}: не удалось разобрать таблицу");
                }
                Ok(resp) => {
                    last_err = format!("{url}: HTTP {}", resp.status().as_u16());
                }
                Err(e) => {
                    last_err = format!("{url}: {e}");
                }
            }
        }
        Err(if last_err.is_empty() {
            "RuTor /top недоступен".to_string()
        } else {
            last_err
        })
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

pub fn format_unix_date_short(ts: i64) -> String {
    if ts <= 0 {
        return String::new();
    }
    if let Some(dt) = DateTime::from_timestamp(ts, 0) {
        let local = dt.with_timezone(&Local);
        return local.format("%d.%m.%Y").to_string();
    }
    String::new()
}

pub fn format_unix_datetime_short(ts: i64) -> String {
    if ts <= 0 {
        return String::new();
    }
    if let Some(dt) = DateTime::from_timestamp(ts, 0) {
        let local = dt.with_timezone(&Local);
        return local.format("%d.%m %H:%M").to_string();
    }
    String::new()
}

pub(crate) fn parse_date_value(v: &Value, keys: &[&str]) -> (i64, String) {
    for k in keys {
        let Some(val) = v.get(*k) else {
            continue;
        };
        if let Some(n) = val.as_i64() {
            let ts = if n > 10_000_000_000 { n / 1000 } else { n };
            if ts > 900_000_000 {
                return (ts, format_unix_date_short(ts));
            }
        }
        if let Some(s) = val.as_str() {
            let (ts, formatted) = parse_date_str(s);
            if ts > 0 {
                return (ts, formatted);
            }
        }
    }
    (0, String::new())
}

pub(crate) fn parse_date_str(raw: &str) -> (i64, String) {
    let s = raw.trim();
    if s.is_empty() {
        return (0, String::new());
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        let ts = dt.timestamp();
        return (ts, format_unix_date_short(ts));
    }
    for fmt in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%d.%m.%Y %H:%M:%S",
        "%d.%m.%Y %H:%M",
    ] {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
            let ts = Utc.from_utc_datetime(&ndt).timestamp();
            return (ts, format_unix_date_short(ts));
        }
    }
    for fmt in ["%Y-%m-%d", "%d.%m.%Y"] {
        if let Ok(nd) = NaiveDate::parse_from_str(s, fmt) {
            if let Some(ndt) = nd.and_hms_opt(12, 0, 0) {
                let ts = Utc.from_utc_datetime(&ndt).timestamp();
                return (ts, format_unix_date_short(ts));
            }
        }
    }
    if let Ok(n) = s.parse::<i64>() {
        let ts = if n > 10_000_000_000 { n / 1000 } else { n };
        if ts > 900_000_000 {
            return (ts, format_unix_date_short(ts));
        }
    }
    (0, String::new())
}

pub(crate) fn detect_quality(title: &str) -> String {
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
    let (date_ts, date_text) = parse_date_value(
        v,
        &["CreateDate", "createDate", "Date", "date", "Timestamp", "timestamp"],
    );
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
        date_ts,
        date_text,
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
    let (date_ts, date_text) = parse_date_value(
        v,
        &["createTime", "updateTime", "CreateTime", "date", "relased"],
    );
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
        date_ts,
        date_text,
    })
}

fn parse_rutor_top_html(html: &str) -> Vec<ApiSearchResult> {
    let mut results = Vec::new();
    let now_ts = Utc::now().timestamp();

    for row in html.split("<tr") {
        let Some(row_end) = row.find("</tr>") else {
            continue;
        };
        let row_html = &row[..row_end];
        if !row_html.contains("magnet:?xt=") {
            continue;
        }
        let Some(mag_pos) = row_html.find("magnet:?xt=") else {
            continue;
        };
        let mag_tail = &row_html[mag_pos..];
        let mag_end = mag_tail
            .find(['"', '\'', '<', '>', ' ', '\n', '\r'])
            .unwrap_or(mag_tail.len());
        let magnet = mag_tail[..mag_end].replace("&amp;", "&");
        if !magnet.starts_with("magnet:?xt=") {
            continue;
        }

        // Extract title from `<a href="/torrent/...`
        let mut title = String::new();
        let mut scan = 0;
        while let Some(a_rel) = row_html[scan..].find("<a ") {
            let a_start = scan + a_rel;
            let Some(gt_rel) = row_html[a_start..].find('>') else {
                break;
            };
            let tag_end = a_start + gt_rel;
            let attrs = &row_html[a_start..tag_end];
            let Some(close_rel) = row_html[tag_end + 1..].find("</a>") else {
                break;
            };
            let inner = &row_html[tag_end + 1..tag_end + 1 + close_rel];
            scan = tag_end + 1 + close_rel + 4;
            if attrs.contains("/torrent/") {
                let cleaned = strip_html_tags(inner);
                if !cleaned.is_empty() {
                    title = cleaned;
                    break;
                }
            }
        }
        if title.is_empty() {
            continue;
        }

        // Split td cells to get date, size, seeds/peers
        let mut cells: Vec<String> = Vec::new();
        for cell_raw in row_html.split("<td") {
            if let Some(gt) = cell_raw.find('>') {
                if let Some(td_end) = cell_raw[gt + 1..].find("</td>") {
                    cells.push(strip_html_tags(&cell_raw[gt + 1..gt + 1 + td_end]));
                }
            }
        }

        let mut size_text = "—".to_string();
        let mut size_bytes = 0.0;
        let mut seeds = 0;
        let mut peers = 0;
        let mut date_text = String::new();
        let mut date_ts = now_ts;

        if let Some(first_cell) = cells.first() {
            let cleaned_date = first_cell.trim();
            if !cleaned_date.is_empty() {
                let (parsed_ts, _) = parse_date_str(cleaned_date);
                if parsed_ts > 0 {
                    date_ts = parsed_ts;
                    date_text = format_unix_date_short(parsed_ts);
                } else {
                    date_text = cleaned_date.to_string();
                }
            }
        }

        for cell in &cells {
            let low = cell.to_lowercase();
            if size_bytes == 0.0
                && (low.contains("gb")
                    || low.contains("mb")
                    || low.contains("гб")
                    || low.contains("мб"))
            {
                let b = parse_size_bytes(cell);
                if b > 0.0 {
                    size_bytes = b;
                    size_text = cell.trim().to_string();
                }
            }
        }

        if let Some(last_cell) = cells.last() {
            let nums: Vec<i32> = last_cell
                .split(|c: char| !c.is_ascii_digit())
                .filter(|s| !s.is_empty())
                .filter_map(|s| s.parse::<i32>().ok())
                .collect();
            if let Some(&s) = nums.first() {
                seeds = s;
            }
            if let Some(&p) = nums.get(1) {
                peers = p;
            }
        }

        let quality = detect_quality(&title);
        results.push(ApiSearchResult {
            title,
            magnet,
            size_text,
            size_bytes,
            seeds,
            peers,
            quality,
            tracker: "RuTor (Топ)".to_string(),
            date_ts,
            date_text,
        });

        if results.len() >= 80 {
            break;
        }
    }

    results
}

fn strip_html_tags(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&nbsp;", " ")
        .replace('\u{00a0}', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn parse_size_bytes(size_str: &str) -> f64 {
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
