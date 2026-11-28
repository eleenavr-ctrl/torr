use crate::core::config::{save_settings, AppConfig, WorkspacePreset};
use crate::core::diagnostics::{crash_log_status, get_logs, run_diagnostics};
use crate::core::paths::AppPaths;
use crate::core::secrets::{FileSecretStore, SecretStore};
use crate::core::storage::PortablePaths;
use crate::modules::ModuleRegistry;
use crate::platform::windows::autostart::WindowsAutostart;
use crate::platform::windows::theme::{apply_dwm_dark_mode, detect_windows_dark_mode};
use crate::platform::windows::window_hooks::extract_hwnd;
use crate::platform::windows::apply_process_priority;
use crate::platform::{Autostart, AutostartMode};
use crate::{AppState, AppWindow, LogEntry, WorkspaceBlockItem};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CHANGELOG_TEXT: &str = include_str!("../CHANGELOG.md");

pub fn slint_app_icon(rgba: &[u8]) -> slint::Image {
    let pixel_buf =
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(rgba, 32, 32);
    slint::Image::from_rgba8(pixel_buf)
}

pub fn show_toast(ui: &AppWindow, message: impl Into<String>) {
    let msg = message.into();
    ui.set_toast_message(msg.into());
    ui.set_toast_visible(true);
    let weak = ui.as_weak();
    slint::Timer::single_shot(Duration::from_secs(3), move || {
        if let Some(app) = weak.upgrade() {
            app.set_toast_visible(false);
        }
    });
}

pub fn apply_theme_and_dwm(app: &AppWindow, mode: i32, hwnd: isize) {
    let sys_dark = detect_windows_dark_mode();
    let is_dark = mode == 2 || (mode == 0 && sys_dark);
    app.invoke_apply_theme(mode, sys_dark);
    apply_dwm_dark_mode(hwnd, is_dark);
}

pub fn apply_language_bundle(lang: &str) {
    let target = if lang == "en" { "en" } else { "ru" };
    let _ = slint::select_bundled_translation(target);
}

pub fn sync_initial_state(
    app: &AppWindow,
    paths: &AppPaths,
    cfg: &AppConfig,
    secrets: &Arc<Mutex<FileSecretStore>>,
) {
    let state = app.global::<AppState>();
    state.set_data_dir_path(paths.user_data_dir.to_string_lossy().to_string().into());
    state.set_data_read_only(paths.read_only);
    state.set_app_lang(if cfg.core.language == "en" { 1 } else { 0 });
    state.set_sync_status_text(
        if cfg.core.webdav.enabled {
            "WebDAV: вкл."
        } else {
            "Синхр.: выкл."
        }
        .into(),
    );

    let auto = WindowsAutostart.status(&paths.exe_path);
    app.set_theme_mode(cfg.core.theme_mode);
    app.set_autostart_mode(auto.active_mode);
    app.set_autostart_needs_repair(auto.needs_repair);
    app.set_start_minimized(cfg.core.start_minimized);
    app.set_close_to_tray(cfg.core.close_to_tray);
    app.set_keep_in_memory(cfg.core.keep_in_memory);
    app.set_profiler_enabled(cfg.core.profiler_enabled);
    app.set_profiler_crash_status(crash_log_status(paths).into());
    app.set_diag_app_version(env!("CARGO_PKG_VERSION").into());
    app.set_changelog_text(CHANGELOG_TEXT.into());

    app.set_webdav_enabled(cfg.core.webdav.enabled);
    app.set_webdav_preset(cfg.core.webdav.preset);
    app.set_webdav_url(cfg.core.webdav.url.clone().into());
    app.set_webdav_user(cfg.core.webdav.username.clone().into());
    app.set_webdav_sync_db(cfg.core.webdav.sync_db);
    app.set_secret_mode(cfg.core.webdav.secret_mode);
    if let Ok(sec) = secrets.lock() {
        if let Ok(Some(pass)) = sec.get_secret("webdav_pass") {
            app.set_webdav_pass(pass.into());
        }
    }

    push_workspace_to_ui(app, cfg);
}

pub fn push_workspace_to_ui(app: &AppWindow, cfg: &AppConfig) {
    let preset_names: Vec<SharedString> = cfg
        .core
        .workspace_presets
        .iter()
        .map(|p| p.name.clone().into())
        .collect();
    let sel_idx = cfg
        .core
        .workspace_presets
        .iter()
        .position(|p| p.name == cfg.core.active_workspace_preset)
        .unwrap_or(0);
    let mut blocks: Vec<WorkspaceBlockItem> = cfg
        .core
        .workspace_presets
        .get(sel_idx)
        .map(|p| {
            let mut sorted = p.blocks.clone();
            sorted.sort_by_key(|b| b.order);
            sorted
                .into_iter()
                .map(|b| WorkspaceBlockItem {
                    id: b.id.into(),
                    module_id: b.module_id.into(),
                    title: b.title.into(),
                    span: b.span,
                    order: b.order,
                })
                .collect()
        })
        .unwrap_or_default();
    if blocks.is_empty() {
        blocks = WorkspacePreset::default()
            .blocks
            .into_iter()
            .map(|b| WorkspaceBlockItem {
                id: b.id.into(),
                module_id: b.module_id.into(),
                title: b.title.into(),
                span: b.span,
                order: b.order,
            })
            .collect();
    }
    let state = app.global::<AppState>();
    state.set_workspace_presets(ModelRc::from(Rc::new(VecModel::from(preset_names))));
    state.set_workspace_selected_preset(sel_idx as i32);
    state.set_workspace_blocks(ModelRc::from(Rc::new(VecModel::from(blocks))));
}

pub fn push_diagnostics_to_ui(
    portable: &PortablePaths,
    config: &Arc<Mutex<AppConfig>>,
    registry: &Arc<Mutex<ModuleRegistry>>,
    ui_weak: &slint::Weak<AppWindow>,
) {
    let cfg = config.lock().map(|c| c.clone()).unwrap_or_default();
    let snapshot = registry
        .lock()
        .map(|r| r.diagnostics_snapshot())
        .unwrap_or_default();
    let modules_list = registry
        .lock()
        .map(|r| r.ui_module_list())
        .unwrap_or_default();

    let report = run_diagnostics(portable, &cfg, &snapshot);
    let logs: Vec<LogEntry> = get_logs()
        .into_iter()
        .map(|e| LogEntry {
            timestamp: e.timestamp.into(),
            tag: e.tag.into(),
            level: e.level.as_str().into(),
            message: e.message.into(),
        })
        .collect();

    let w = ui_weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = w.upgrade() {
            app.set_diag_app_version(report.app_version.into());
            app.set_diag_engine_version(report.engine_version.into());
            app.set_diag_binary_status(report.binary_status.into());
            app.set_diag_port_status(report.port_status.into());
            app.set_diag_api_ping(report.api_ping.into());
            app.set_diag_problem_hint(report.problem_hint.into());
            app.set_diag_has_problem(report.has_problem);
            app.set_diag_logs(ModelRc::from(Rc::new(VecModel::from(logs))));
            app.set_registered_modules(ModelRc::from(Rc::new(VecModel::from(modules_list))));
        }
    });
}

pub fn bind_core_callbacks(
    app: &AppWindow,
    paths: &AppPaths,
    portable: &PortablePaths,
    config: &Arc<Mutex<AppConfig>>,
    registry: &Arc<Mutex<ModuleRegistry>>,
    secrets: &Arc<Mutex<FileSecretStore>>,
) {
    // Theme & Language
    {
        let p = paths.clone();
        let cfg = Arc::clone(config);
        let weak = app.as_weak();
        app.on_change_theme_mode(move |mode| {
            if let Ok(mut g) = cfg.lock() {
                g.core.theme_mode = mode;
                let _ = save_settings(&p, &g);
            }
            if let Some(a) = weak.upgrade() {
                apply_theme_and_dwm(&a, mode, extract_hwnd(a.window()));
            }
        });
    }
    {
        let p = paths.clone();
        let cfg = Arc::clone(config);
        let weak = app.as_weak();
        app.on_change_language(move |idx| {
            let lang = if idx == 1 { "en" } else { "ru" };
            apply_language_bundle(lang);
            if let Ok(mut g) = cfg.lock() {
                g.core.language = lang.into();
                let _ = save_settings(&p, &g);
            }
            if let Some(a) = weak.upgrade() {
                show_toast(&a, if idx == 1 { "Language: English" } else { "Язык: Русский" });
            }
        });
    }

    // Dual Autostart & Repair
    {
        let p = paths.clone();
        let cfg = Arc::clone(config);
        let weak = app.as_weak();
        app.on_set_autostart_mode(move |mode_idx| {
            let min = cfg.lock().map(|g| g.core.start_minimized).unwrap_or(false);
            let res = WindowsAutostart.apply(AutostartMode::from_i32(mode_idx), &p.exe_path, min);
            if let Ok(mut g) = cfg.lock() {
                g.core.autostart_mode = mode_idx;
                g.core.autostart = mode_idx != 0;
                let _ = save_settings(&p, &g);
            }
            if let Some(a) = weak.upgrade() {
                a.set_autostart_needs_repair(false);
                match res {
                    Ok(_) => show_toast(&a, "Настройки автозапуска обновлены"),
                    Err(e) => show_toast(&a, e),
                }
            }
        });
    }
    {
        let p = paths.clone();
        let cfg = Arc::clone(config);
        let weak = app.as_weak();
        app.on_repair_autostart(move || {
            let (mode, min) = cfg
                .lock()
                .map(|g| (g.core.autostart_mode, g.core.start_minimized))
                .unwrap_or((1, false));
            let res = WindowsAutostart.repair(AutostartMode::from_i32(mode), &p.exe_path, min);
            if let Some(a) = weak.upgrade() {
                if res.is_ok() {
                    a.set_autostart_needs_repair(false);
                    show_toast(&a, "Путь автозапуска исправлен");
                } else if let Err(e) = res {
                    show_toast(&a, e);
                }
            }
        });
    }

    // Start minimized, Close-to-tray, Keep in memory
    {
        let p = paths.clone();
        let cfg = Arc::clone(config);
        app.on_toggle_start_minimized(move |val| {
            if let Ok(mut g) = cfg.lock() {
                g.core.start_minimized = val;
                let _ = save_settings(&p, &g);
                let mode = g.core.autostart_mode;
                if mode != 0 {
                    let _ = WindowsAutostart.apply(AutostartMode::from_i32(mode), &p.exe_path, val);
                }
            }
        });
    }
    {
        let p = paths.clone();
        let cfg = Arc::clone(config);
        app.on_toggle_close_to_tray(move |val| {
            if let Ok(mut g) = cfg.lock() {
                g.core.close_to_tray = val;
                let _ = save_settings(&p, &g);
            }
        });
    }
    {
        let p = paths.clone();
        let cfg = Arc::clone(config);
        let reg = Arc::clone(registry);
        let weak = app.as_weak();
        app.on_toggle_keep_in_memory(move |val| {
            if let Ok(mut g) = cfg.lock() {
                g.core.keep_in_memory = val;
                let _ = save_settings(&p, &g);
            }
            apply_process_priority(val);
            if let Ok(r) = reg.lock() {
                r.dispatch_action(if val { "priority_high" } else { "priority_normal" });
            }
            if let Some(a) = weak.upgrade() {
                show_toast(&a, if val { "Приоритет повышен" } else { "Стандартный приоритет" });
            }
        });
    }

    crate::ui_services::bind_services_and_workspace(app, paths, portable, config, registry, secrets);
}
