use std::path::Path;

pub mod windows;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartMode {
    Off = 0,
    StartupShortcut = 1,
    RegistryRun = 2,
}

impl AutostartMode {
    pub fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::StartupShortcut,
            2 => Self::RegistryRun,
            _ => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AutostartStatus {
    pub active_mode: i32,
    pub needs_repair: bool,
    pub registered_target: String,
}

#[derive(Debug, Clone, Default)]
pub struct ProcessMetricsSnapshot {
    pub cpu_percent: f32,
    pub working_set_bytes: u64,
    pub private_bytes: u64,
    pub handle_count: u32,
    pub gdi_objects: u32,
    pub user_objects: u32,
}

pub trait PathsProvider: Send + Sync {
    fn executable_path(&self) -> Result<std::path::PathBuf, String>;
}

pub trait TrayService: Send + Sync {
    fn shutdown(&mut self);
}

pub trait SingleInstance: Send + Sync {
    fn release(&mut self);
}

pub trait ProcessMetrics: Send + Sync {
    fn sample(&mut self) -> ProcessMetricsSnapshot;
}

pub trait Autostart: Send + Sync {
    fn status(&self, current_exe: &Path) -> AutostartStatus;
    fn apply(&self, mode: AutostartMode, current_exe: &Path, start_minimized: bool) -> Result<(), String>;
    fn repair(&self, mode: AutostartMode, current_exe: &Path, start_minimized: bool) -> Result<(), String>;
}

pub trait SystemTheme: Send + Sync {
    fn is_dark(&self) -> bool;
    fn accent_rgb(&self) -> (u8, u8, u8);
    fn apply_window_theme(&self, window_handle: isize, is_dark: bool);
}
