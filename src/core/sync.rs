use crate::core::config::{merge_configs, AppConfig};
use crate::core::db::AppDatabase;
use crate::core::error::{CoreError, CoreResult};
use crate::core::paths::AppPaths;
use reqwest::blocking::Client;
use reqwest::Method;
use std::fs;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictResolution {
    KeepLocal,
    UseRemote,
}

#[derive(Debug, Clone)]
pub enum SyncOutcome {
    Synced(AppConfig, String),
    ConflictPrompt {
        merged_config: AppConfig,
        summary: String,
    },
}

pub trait SyncProvider: Send + Sync {
    fn test_connection(&self) -> CoreResult<String>;
    fn sync_now(
        &self,
        paths: &AppPaths,
        local_cfg: &AppConfig,
        sync_db: bool,
    ) -> CoreResult<SyncOutcome>;
    fn resolve_db_conflict(
        &self,
        paths: &AppPaths,
        choice: ConflictResolution,
    ) -> CoreResult<String>;
}

pub struct WebDavSyncProvider {
    pub base_url: String,
    pub username: String,
    pub password: String,
    client: Client,
}

impl WebDavSyncProvider {
    pub fn new(base_url: &str, username: &str, password: &str) -> CoreResult<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| CoreError::Sync(format!("HTTP client error: {e}")))?;
        Ok(Self {
            base_url: normalize_webdav_root(base_url),
            username: username.trim().to_string(),
            password: password.to_string(),
            client,
        })
    }

    fn folder_url(&self) -> String {
        format!("{}/TorrServerUI", self.base_url)
    }

    fn file_url(&self, name: &str) -> String {
        format!("{}/{}", self.folder_url(), name)
    }

    fn ensure_remote_folder(&self) -> CoreResult<()> {
        let mkcol = Method::from_bytes(b"MKCOL").map_err(|e| CoreError::Sync(e.to_string()))?;
        let resp = self
            .client
            .request(mkcol, self.folder_url())
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .map_err(|e| CoreError::Sync(e.to_string()))?;
        let code = resp.status().as_u16();
        // 201 Created, 405 Already Exists, 301/200 OK
        if code == 201 || code == 405 || code == 200 || code == 301 {
            Ok(())
        } else if code == 401 || code == 403 {
            Err(CoreError::Sync(format!("Ошибка авторизации WebDAV (HTTP {code})")))
        } else {
            Ok(())
        }
    }

    fn acquire_lock(&self, device_id: &str) -> CoreResult<()> {
        let lock_url = self.file_url(".sync.lock");
        if let Ok(resp) = self
            .client
            .get(&lock_url)
            .basic_auth(&self.username, Some(&self.password))
            .send()
        {
            if resp.status().is_success() {
                let body = resp.text().unwrap_or_default();
                if let Some((holder, ts_str)) = body.split_once('|') {
                    let ts = ts_str.trim().parse::<i64>().unwrap_or(0);
                    let age = chrono::Utc::now().timestamp() - ts;
                    if holder != device_id && age < 120 {
                        return Err(CoreError::Sync(format!(
                            "Облако занято другим устройством ({holder})"
                        )));
                    }
                }
            }
        }
        let payload = format!("{device_id}|{}", chrono::Utc::now().timestamp());
        let resp = self
            .client
            .put(&lock_url)
            .basic_auth(&self.username, Some(&self.password))
            .body(payload)
            .send()
            .map_err(|e| CoreError::Sync(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(CoreError::Sync(format!(
                "Не удалось установить маркер блокировки (HTTP {})",
                resp.status()
            )));
        }
        Ok(())
    }

    fn release_lock(&self) {
        let _ = self
            .client
            .delete(self.file_url(".sync.lock"))
            .basic_auth(&self.username, Some(&self.password))
            .send();
    }
}

impl SyncProvider for WebDavSyncProvider {
    fn test_connection(&self) -> CoreResult<String> {
        let propfind = Method::from_bytes(b"PROPFIND").map_err(|e| CoreError::Sync(e.to_string()))?;
        let resp = self
            .client
            .request(propfind, &self.base_url)
            .header("Depth", "0")
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .map_err(|e| CoreError::Sync(format!("Ошибка подключения: {e}")))?;
        let st = resp.status().as_u16();
        if (200..300).contains(&st) || st == 207 {
            self.ensure_remote_folder()?;
            Ok(format!("Подключение WebDAV OK (HTTP {st})"))
        } else {
            Err(CoreError::Sync(format!("Сервер ответил HTTP {st}")))
        }
    }

    fn sync_now(
        &self,
        paths: &AppPaths,
        local_cfg: &AppConfig,
        sync_db: bool,
    ) -> CoreResult<SyncOutcome> {
        if paths.read_only {
            return Err(CoreError::ReadOnly);
        }
        self.ensure_remote_folder()?;
        self.acquire_lock(&local_cfg.device_id)?;

        let result = (|| -> CoreResult<SyncOutcome> {
            let settings_url = self.file_url("settings.json");
            let remote_cfg: Option<AppConfig> = match self
                .client
                .get(&settings_url)
                .basic_auth(&self.username, Some(&self.password))
                .send()
            {
                Ok(r) if r.status().is_success() => {
                    let text = r.text().unwrap_or_default();
                    serde_json::from_str::<AppConfig>(&text).ok()
                }
                _ => None,
            };

            let merged = match remote_cfg {
                Some(ref rem) => merge_configs(local_cfg, rem),
                None => local_cfg.clone(),
            };

            let body = serde_json::to_string_pretty(&merged)?;
            let put_resp = self
                .client
                .put(&settings_url)
                .basic_auth(&self.username, Some(&self.password))
                .body(body)
                .send()
                .map_err(|e| CoreError::Sync(e.to_string()))?;
            if !put_resp.status().is_success() {
                return Err(CoreError::Sync(format!(
                    "Ошибка выгрузки settings.json: HTTP {}",
                    put_resp.status()
                )));
            }

            if sync_db {
                let db_url = self.file_url("app_data.db");
                let remote_head = self
                    .client
                    .head(&db_url)
                    .basic_auth(&self.username, Some(&self.password))
                    .send();

                if let Ok(head_r) = remote_head {
                    if head_r.status().is_success() {
                        let remote_len = head_r
                            .headers()
                            .get("content-length")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(0);
                        let local_len = fs::metadata(&paths.db_path).map(|m| m.len()).unwrap_or(0);
                        if local_len > 0 && remote_len > 0 && remote_len != local_len {
                            return Ok(SyncOutcome::ConflictPrompt {
                                merged_config: merged,
                                summary: format!(
                                    "Обнаружена отличающаяся база в облаке (локально: {local_len} Б, облако: {remote_len} Б). Что использовать?"
                                ),
                            });
                        }
                    }
                }

                let db = AppDatabase::open(&paths.db_path, false)?;
                let snap = db.snapshot_to(&paths.backups_dir, "webdav_upload.db")?;
                db.checkpoint_and_close();
                let bytes = fs::read(&snap)?;
                let db_put = self
                    .client
                    .put(&db_url)
                    .basic_auth(&self.username, Some(&self.password))
                    .body(bytes)
                    .send()
                    .map_err(|e| CoreError::Sync(e.to_string()))?;
                if !db_put.status().is_success() {
                    return Err(CoreError::Sync(format!(
                        "Ошибка выгрузки БД: HTTP {}",
                        db_put.status()
                    )));
                }
            }

            Ok(SyncOutcome::Synced(
                merged,
                "Синхронизация WebDAV завершена".into(),
            ))
        })();

        self.release_lock();
        result
    }

    fn resolve_db_conflict(
        &self,
        paths: &AppPaths,
        choice: ConflictResolution,
    ) -> CoreResult<String> {
        let db_url = self.file_url("app_data.db");
        match choice {
            ConflictResolution::KeepLocal => {
                let db = AppDatabase::open(&paths.db_path, false)?;
                let snap = db.snapshot_to(&paths.backups_dir, "webdav_upload.db")?;
                db.checkpoint_and_close();
                let bytes = fs::read(&snap)?;
                let resp = self
                    .client
                    .put(&db_url)
                    .basic_auth(&self.username, Some(&self.password))
                    .body(bytes)
                    .send()
                    .map_err(|e| CoreError::Sync(e.to_string()))?;
                if !resp.status().is_success() {
                    return Err(CoreError::Sync(format!("HTTP {}", resp.status())));
                }
                Ok("Локальная база загружена в WebDAV".into())
            }
            ConflictResolution::UseRemote => {
                // Local safety backup first
                if paths.db_path.exists() {
                    let db = AppDatabase::open(&paths.db_path, false)?;
                    let _ = db.snapshot_to(
                        &paths.backups_dir,
                        &format!("pre_sync_{}.db", chrono::Utc::now().timestamp()),
                    );
                    db.checkpoint_and_close();
                }
                let resp = self
                    .client
                    .get(&db_url)
                    .basic_auth(&self.username, Some(&self.password))
                    .send()
                    .map_err(|e| CoreError::Sync(e.to_string()))?;
                if !resp.status().is_success() {
                    return Err(CoreError::Sync(format!("HTTP {}", resp.status())));
                }
                let bytes = resp.bytes().map_err(|e| CoreError::Sync(e.to_string()))?;
                let tmp = paths.db_path.with_extension("db.tmp");
                fs::write(&tmp, &bytes)?;
                if paths.db_path.exists() {
                    let _ = fs::remove_file(&paths.db_path);
                }
                fs::rename(&tmp, &paths.db_path)?;
                Ok("База восстановлена из WebDAV (резервная копия сохранена в Backups/)".into())
            }
        }
    }
}

fn normalize_webdav_root(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        "https://webdav.yandex.ru".to_string()
    } else if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}
