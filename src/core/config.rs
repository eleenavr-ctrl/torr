use crate::core::error::{CoreError, CoreResult};
use crate::core::paths::AppPaths;
use crate::core::storage::TorrServerConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

pub const CURRENT_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceBlockConfig {
    pub id: String,
    pub module_id: String,
    pub title: String,
    pub span: i32,
    pub order: i32,
}

impl Default for WorkspaceBlockConfig {
    fn default() -> Self {
        Self {
            id: "torrents".into(),
            module_id: "torrents".into(),
            title: "Раздачи".into(),
            span: 2,
            order: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspacePreset {
    pub name: String,
    pub blocks: Vec<WorkspaceBlockConfig>,
    pub updated_at: i64,
}

impl Default for WorkspacePreset {
    fn default() -> Self {
        Self {
            name: "Стандартный".into(),
            updated_at: 0,
            blocks: vec![
                WorkspaceBlockConfig {
                    id: "blk_torrents".into(),
                    module_id: "torrents".into(),
                    title: "Раздачи TorrServer".into(),
                    span: 2,
                    order: 0,
                },
                WorkspaceBlockConfig {
                    id: "blk_search".into(),
                    module_id: "search".into(),
                    title: "Быстрый поиск".into(),
                    span: 1,
                    order: 1,
                },
                WorkspaceBlockConfig {
                    id: "blk_history".into(),
                    module_id: "history".into(),
                    title: "История просмотров".into(),
                    span: 3,
                    order: 2,
                },
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebDavConfig {
    pub enabled: bool,
    pub preset: i32,
    pub url: String,
    pub username: String,
    pub sync_db: bool,
    pub secret_mode: i32, // 0 = DPAPI, 1 = MasterPassword
}

impl Default for WebDavConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            preset: 0,
            url: "https://webdav.yandex.ru".into(),
            username: String::new(),
            sync_db: false,
            secret_mode: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CoreConfig {
    pub theme_mode: i32,
    pub language: String,
    pub autostart: bool,
    pub autostart_mode: i32, // 0=Off, 1=Startup .lnk, 2=HKCU Run
    pub start_minimized: bool,
    pub close_to_tray: bool,
    pub keep_in_memory: bool,
    pub profiler_enabled: bool,
    pub window_geometry: WindowGeometry,
    pub webdav: WebDavConfig,
    pub workspace_presets: Vec<WorkspacePreset>,
    pub active_workspace_preset: String,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            theme_mode: 0,
            language: "ru".into(),
            autostart: false,
            autostart_mode: 0,
            start_minimized: false,
            close_to_tray: true,
            keep_in_memory: true,
            profiler_enabled: false,
            window_geometry: WindowGeometry::default(),
            webdav: WebDavConfig::default(),
            workspace_presets: vec![WorkspacePreset::default()],
            active_workspace_preset: "Стандартный".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub schema_version: u32,
    pub updated_at: i64,
    pub device_id: String,
    pub core: CoreConfig,
    pub torrserver: TorrServerConfig,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            updated_at: chrono::Utc::now().timestamp(),
            device_id: default_device_id(),
            core: CoreConfig::default(),
            torrserver: TorrServerConfig::default(),
            extra: BTreeMap::new(),
        }
    }
}

fn default_device_id() -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "portable".into());
    format!("{host}-{}", std::process::id())
}

pub fn load_settings(paths: &AppPaths) -> CoreResult<AppConfig> {
    if !paths.settings_path.exists() {
        let cfg = AppConfig::default();
        if !paths.read_only {
            let _ = save_settings(paths, &cfg);
        }
        return Ok(cfg);
    }

    let raw = fs::read_to_string(&paths.settings_path)?;
    let mut cfg: AppConfig = serde_json::from_str(&raw)?;
    if cfg.schema_version > CURRENT_SCHEMA_VERSION {
        return Err(CoreError::SchemaDowngrade {
            found: cfg.schema_version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }
    if cfg.schema_version < CURRENT_SCHEMA_VERSION {
        cfg.schema_version = CURRENT_SCHEMA_VERSION;
        if cfg.core.workspace_presets.is_empty() {
            cfg.core.workspace_presets = vec![WorkspacePreset::default()];
        }
    }
    if cfg.device_id.is_empty() {
        cfg.device_id = default_device_id();
    }
    Ok(cfg)
}

pub fn save_settings(paths: &AppPaths, config: &AppConfig) -> CoreResult<()> {
    if paths.read_only {
        return Err(CoreError::ReadOnly);
    }
    let mut to_save = config.clone();
    to_save.schema_version = CURRENT_SCHEMA_VERSION;
    to_save.updated_at = chrono::Utc::now().timestamp();
    atomic_write_json(&paths.settings_path, &to_save)
}

pub fn atomic_write_json<T: Serialize>(target: &Path, value: &T) -> CoreResult<()> {
    let json = serde_json::to_string_pretty(value)?;
    let tmp = target.with_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
    }
    if target.exists() {
        let _ = fs::remove_file(target);
    }
    fs::rename(&tmp, target)?;
    Ok(())
}

pub fn merge_configs(local: &AppConfig, remote: &AppConfig) -> AppConfig {
    let mut merged = if remote.updated_at > local.updated_at {
        remote.clone()
    } else {
        local.clone()
    };

    // Merge workspace presets by name + newest updated_at
    let mut preset_map: BTreeMap<String, WorkspacePreset> = BTreeMap::new();
    for p in &local.core.workspace_presets {
        preset_map.insert(p.name.clone(), p.clone());
    }
    for p in &remote.core.workspace_presets {
        match preset_map.get(&p.name) {
            Some(existing) if existing.updated_at >= p.updated_at => {}
            _ => {
                preset_map.insert(p.name.clone(), p.clone());
            }
        }
    }
    if !preset_map.is_empty() {
        merged.core.workspace_presets = preset_map.into_values().collect();
    }

    // Preserve unknown extra keys from both sides
    for (k, v) in &local.extra {
        merged.extra.entry(k.clone()).or_insert_with(|| v.clone());
    }
    for (k, v) in &remote.extra {
        if remote.updated_at >= local.updated_at {
            merged.extra.insert(k.clone(), v.clone());
        } else {
            merged.extra.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    merged.updated_at = local.updated_at.max(remote.updated_at);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_unknown_fields_and_guards_downgrade() {
        let base = std::env::temp_dir().join(format!("ts_cfg_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let paths = AppPaths::from_root(base.join("app.exe"), base.clone(), base.join("UserData"))
            .unwrap();

        fs::write(
            &paths.settings_path,
            r#"{"schema_version":1,"future_custom_field":{"x":42},"core":{"theme_mode":2}}"#,
        )
        .unwrap();

        let cfg = load_settings(&paths).expect("load v1");
        assert_eq!(cfg.core.theme_mode, 2);
        assert!(cfg.extra.contains_key("future_custom_field"));
        save_settings(&paths, &cfg).expect("atomic save");
        let saved_raw = fs::read_to_string(&paths.settings_path).unwrap();
        assert!(saved_raw.contains("future_custom_field"));

        fs::write(&paths.settings_path, r#"{"schema_version":999}"#).unwrap();
        let err = load_settings(&paths).unwrap_err();
        assert!(matches!(err, CoreError::SchemaDowngrade { .. }));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn merges_presets_by_name_and_timestamp() {
        let mut a = AppConfig::default();
        a.updated_at = 100;
        a.core.workspace_presets = vec![WorkspacePreset {
            name: "Кино".into(),
            blocks: vec![],
            updated_at: 200,
        }];
        let mut b = AppConfig::default();
        b.updated_at = 150;
        b.core.workspace_presets = vec![WorkspacePreset {
            name: "Работа".into(),
            blocks: vec![],
            updated_at: 180,
        }];
        let m = merge_configs(&a, &b);
        assert_eq!(m.core.workspace_presets.len(), 2);
    }
}
