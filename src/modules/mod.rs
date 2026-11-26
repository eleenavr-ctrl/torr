pub mod torrserver;

use crate::core::diagnostics;
use crate::{AppWindow, ModuleListItem};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ModuleInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct ModuleStatus {
    pub healthy: bool,
    pub state_label: String,
    pub version_label: String,
    pub speed_label: String,
    pub active_count: i32,
}

impl Default for ModuleStatus {
    fn default() -> Self {
        Self {
            healthy: false,
            state_label: "Ожидание".to_string(),
            version_label: String::new(),
            speed_label: "0 KB/s".to_string(),
            active_count: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExternalModuleInfo {
    pub name: String,
    pub path: PathBuf,
}

pub trait AppModule: Send + Sync {
    fn info(&self) -> ModuleInfo;
    fn on_init(&mut self);
    fn bind_ui(&self, ui: &AppWindow);
    fn on_window_visibility_changed(&self, visible: bool);
    fn on_shutdown(&mut self);
    fn status_line(&self) -> ModuleStatus;
    fn diagnostics_snapshot(&self) -> String;

    fn handle_dropped_files(&self, _paths: &[PathBuf]) -> bool {
        false
    }

    fn handle_action(&self, _action: &str) -> bool {
        false
    }
}

pub struct ModuleRegistry {
    modules: Vec<Box<dyn AppModule>>,
    external_modules: Vec<ExternalModuleInfo>,
}

impl ModuleRegistry {
    pub fn new(modules_dir: &Path) -> Self {
        let external_modules = scan_external_modules(modules_dir);
        if !external_modules.is_empty() {
            diagnostics::info(
                "[CORE]",
                format!(
                    "Обнаружено внешних модулей в data/modules/: {}",
                    external_modules.len()
                ),
            );
        }
        Self {
            modules: Vec::new(),
            external_modules,
        }
    }

    pub fn register(&mut self, module: Box<dyn AppModule>) {
        let info = module.info();
        diagnostics::info(
            "[CORE]",
            format!("Регистрация модуля: {} (id={}, v{})", info.name, info.id, info.version),
        );
        self.modules.push(module);
    }

    pub fn init_all(&mut self) {
        for m in &mut self.modules {
            if m.info().enabled {
                m.on_init();
            }
        }
    }

    pub fn bind_all_ui(&self, ui: &AppWindow) {
        for m in &self.modules {
            if m.info().enabled {
                m.bind_ui(ui);
            }
        }
    }

    pub fn notify_visibility(&self, visible: bool) {
        for m in &self.modules {
            if m.info().enabled {
                m.on_window_visibility_changed(visible);
            }
        }
    }

    pub fn shutdown_all(&mut self) {
        for m in &mut self.modules {
            m.on_shutdown();
        }
    }

    pub fn dispatch_dropped_files(&self, paths: &[PathBuf]) -> bool {
        for m in &self.modules {
            if m.info().enabled && m.handle_dropped_files(paths) {
                return true;
            }
        }
        false
    }

    pub fn dispatch_action(&self, action: &str) -> bool {
        for m in &self.modules {
            if m.info().enabled && m.handle_action(action) {
                return true;
            }
        }
        false
    }

    pub fn primary_status(&self) -> ModuleStatus {
        self.modules
            .first()
            .map(|m| m.status_line())
            .unwrap_or_default()
    }

    pub fn diagnostics_snapshot(&self) -> String {
        let mut lines = Vec::new();
        for m in &self.modules {
            let info = m.info();
            let status = m.status_line();
            lines.push(format!(
                "[{}] {} v{} | enabled={} | state={}\n{}",
                info.id,
                info.name,
                info.version,
                info.enabled,
                status.state_label,
                m.diagnostics_snapshot()
            ));
        }
        if !self.external_modules.is_empty() {
            for ext in &self.external_modules {
                lines.push(format!(
                    "[external] {} ({})",
                    ext.name,
                    ext.path.display()
                ));
            }
        }
        lines.join("\n")
    }

    pub fn ui_module_list(&self) -> Vec<ModuleListItem> {
        let mut list = Vec::new();
        for m in &self.modules {
            let info = m.info();
            let st = m.status_line();
            list.push(ModuleListItem {
                id: info.id.into(),
                name: info.name.into(),
                version: info.version.into(),
                description: info.description.into(),
                status: st.state_label.into(),
                enabled: info.enabled,
                is_external: false,
            });
        }
        for ext in &self.external_modules {
            list.push(ModuleListItem {
                id: "external".into(),
                name: ext.name.clone().into(),
                version: "ext".into(),
                description: ext.path.to_string_lossy().to_string().into(),
                status: "Обнаружен".into(),
                enabled: true,
                is_external: true,
            });
        }
        list
    }
}

fn scan_external_modules(modules_dir: &Path) -> Vec<ExternalModuleInfo> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(modules_dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if ext.eq_ignore_ascii_case("exe") {
                    let name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("module")
                        .to_string();
                    found.push(ExternalModuleInfo { name, path });
                }
            }
        }
    }
    found
}
