pub mod core;
pub mod modules;
pub mod platform;
pub mod services;
pub mod ui_bridge;
pub mod ui_detached;
pub mod ui_services;

use crate::core::config::{load_settings, save_settings};
use crate::core::db::AppDatabase;
use crate::core::diagnostics::{self, init_logging};
use crate::core::paths::AppPaths;
use crate::core::secrets::FileSecretStore;
use crate::core::storage::PortablePaths;
use crate::modules::torrserver::TorrServerModule;
use crate::modules::ModuleRegistry;
use crate::platform::windows::apply_process_priority;
use crate::platform::windows::single_instance::WindowsSingleInstance;
use crate::platform::windows::tray::{TrayCommand, TrayHandle};
use crate::platform::windows::window_hooks::{
    bring_hwnd_to_front, extract_hwnd, install_win32_subclass, read_window_geometry,
    restore_window_geometry,
};
use crate::platform::SingleInstance;
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

slint::include_modules!();

fn apply_hwnd_setup(
    app: &AppWindow,
    config: &Arc<Mutex<crate::core::config::AppConfig>>,
    registry: &Arc<Mutex<ModuleRegistry>>,
    last_hwnd: &Arc<AtomicIsize>,
    bring_front: bool,
) -> bool {
    let hwnd = extract_hwnd(app.window());
    if hwnd == 0 {
        return false;
    }
    if last_hwnd.load(Ordering::Relaxed) != hwnd {
        let (geom, theme) = config
            .lock()
            .map(|c| (c.core.window_geometry.clone(), c.core.theme_mode))
            .unwrap_or_default();
        restore_window_geometry(hwnd, &geom);
        ui_bridge::apply_theme_and_dwm(app, theme, hwnd);
        install_win32_subclass(hwnd, Arc::clone(registry));
        last_hwnd.store(hwnd, Ordering::Relaxed);
    }
    if bring_front {
        bring_hwnd_to_front(hwnd);
    }
    true
}

fn show_and_configure_window(
    app: &AppWindow,
    config: &Arc<Mutex<crate::core::config::AppConfig>>,
    registry: &Arc<Mutex<ModuleRegistry>>,
    window_visible: &Arc<AtomicBool>,
    last_hwnd: &Arc<AtomicIsize>,
    bring_front: bool,
) {
    window_visible.store(true, Ordering::Relaxed);
    let _ = app.show();
    if let Ok(g) = registry.lock() {
        g.notify_visibility(true);
    }
    if !apply_hwnd_setup(app, config, registry, last_hwnd, bring_front) {
        let weak = app.as_weak();
        let cfg_c = Arc::clone(config);
        let reg_c = Arc::clone(registry);
        let hwnd_c = Arc::clone(last_hwnd);
        slint::Timer::single_shot(Duration::from_millis(30), move || {
            if let Some(a) = weak.upgrade() {
                let _ = apply_hwnd_setup(&a, &cfg_c, &reg_c, &hwnd_c, bring_front);
            }
        });
    }
}

pub fn run() {
    let cli_args: Vec<String> = std::env::args().collect();
    let paths = match AppPaths::init(&cli_args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Fatal paths init error: {e}");
            return;
        }
    };

    init_logging(&paths);
    let _ = tracing_subscriber::fmt().with_target(false).try_init();

    let window_visible = Arc::new(AtomicBool::new(true));
    let last_hwnd = Arc::new(AtomicIsize::new(0));
    let ui_weak_holder: Arc<Mutex<Option<slint::Weak<AppWindow>>>> = Arc::new(Mutex::new(None));
    let ctx_holder: Arc<Mutex<Option<(Arc<Mutex<crate::core::config::AppConfig>>, Arc<Mutex<ModuleRegistry>>)>>> =
        Arc::new(Mutex::new(None));

    let instance = {
        let vis = Arc::clone(&window_visible);
        let holder = Arc::clone(&ui_weak_holder);
        let ctx_s = Arc::clone(&ctx_holder);
        let hwnd_s = Arc::clone(&last_hwnd);
        WindowsSingleInstance::acquire(&paths.user_data_dir, move || {
            let Some(weak) = holder.lock().ok().and_then(|g| g.clone()) else {
                return;
            };
            let Some((cfg_c, reg_c)) = ctx_s.lock().ok().and_then(|g| g.clone()) else {
                return;
            };
            let vis_c = Arc::clone(&vis);
            let hwnd_c = Arc::clone(&hwnd_s);
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = weak.upgrade() {
                    show_and_configure_window(&app, &cfg_c, &reg_c, &vis_c, &hwnd_c, true);
                }
            });
        })
    };
    let Some(mut single_instance) = instance else {
        return;
    };

    diagnostics::info(
        "[CORE]",
        format!(
            "Запуск TorrServerUI v{} (UserData: {}, RO={})",
            env!("CARGO_PKG_VERSION"),
            paths.user_data_dir.display(),
            paths.read_only
        ),
    );

    let db = match AppDatabase::open(&paths.db_path, paths.read_only) {
        Ok(d) => Some(d),
        Err(e) => {
            diagnostics::warn("[CORE]", format!("SQLite fallback (RO/warn): {e}"));
            None
        }
    };

    let loaded_cfg = match load_settings(&paths) {
        Ok(c) => c,
        Err(e) => {
            diagnostics::warn("[CORE]", format!("Settings warning: {e}"));
            Default::default()
        }
    };

    ui_bridge::apply_language_bundle(&loaded_cfg.core.language);
    let cli_minimized = cli_args.iter().any(|a| a == "--minimized" || a == "-m");
    let launch_minimized = cli_minimized || loaded_cfg.core.start_minimized;
    window_visible.store(!launch_minimized, Ordering::Relaxed);

    apply_process_priority(loaded_cfg.core.keep_in_memory);

    let secrets = Arc::new(Mutex::new(
        FileSecretStore::open(&paths.secrets_path, paths.read_only, None).unwrap_or_else(|_| {
            FileSecretStore::open(&paths.secrets_path, true, None)
                .unwrap_or_else(|_| unreachable!())
        }),
    ));

    let portable = PortablePaths::from(paths.clone());
    let config = Arc::new(Mutex::new(loaded_cfg));

    let mut reg = ModuleRegistry::new(&portable.modules_dir);
    reg.register(Box::new(TorrServerModule::new(
        portable.clone(),
        Arc::clone(&config),
    )));
    reg.init_all();
    let registry = Arc::new(Mutex::new(reg));

    if let Ok(mut g) = ctx_holder.lock() {
        *g = Some((Arc::clone(&config), Arc::clone(&registry)));
    }

    let app = AppWindow::new().expect("Не удалось создать AppWindow");
    if let Ok(mut g) = ui_weak_holder.lock() {
        *g = Some(app.as_weak());
    }

    let icon_rgba = core::generate_app_icon_rgba();
    let app_icon = ui_bridge::slint_app_icon(&icon_rgba);
    app.set_app_icon(app_icon.clone());

    if let Ok(cfg_g) = config.lock() {
        ui_bridge::sync_initial_state(&app, &paths, &cfg_g, &secrets);
        ui_bridge::apply_theme_and_dwm(&app, cfg_g.core.theme_mode, 0);
    }

    if let Ok(r) = registry.lock() {
        r.bind_all_ui(&app);
        app.set_registered_modules(ModelRc::from(Rc::new(VecModel::from(r.ui_module_list()))));
    }

    ui_bridge::bind_core_callbacks(&app, &paths, &portable, &config, &registry, &secrets);
    let detached_holder = Rc::new(RefCell::new(None));
    ui_detached::bind_detach_callbacks(&app, app_icon, detached_holder);

    let mut tray_handle = {
        let ui_weak = Arc::new(Mutex::new(app.as_weak()));
        let reg_tray = Arc::clone(&registry);
        let vis_tray = Arc::clone(&window_visible);
        let cfg_tray = Arc::clone(&config);
        let hwnd_tray = Arc::clone(&last_hwnd);
        TrayHandle::spawn(icon_rgba, move |cmd| match cmd {
            TrayCommand::ToggleWindow | TrayCommand::ShowWindow => {
                let show_only = cmd == TrayCommand::ShowWindow;
                let Some(w) = ui_weak.lock().ok().map(|g| g.clone()) else { return };
                let r = Arc::clone(&reg_tray);
                let v = Arc::clone(&vis_tray);
                let c = Arc::clone(&cfg_tray);
                let h = Arc::clone(&hwnd_tray);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = w.upgrade() {
                        if !show_only && v.load(Ordering::Relaxed) {
                            v.store(false, Ordering::Relaxed);
                            let _ = app.hide();
                            if let Ok(g) = r.lock() { g.notify_visibility(false); }
                        } else {
                            show_and_configure_window(&app, &c, &r, &v, &h, true);
                        }
                    }
                });
            }
            TrayCommand::RestartServer => {
                if let Ok(g) = reg_tray.lock() { g.dispatch_action("restart_server"); }
            }
            TrayCommand::OpenWebPanel => {
                if let Ok(g) = reg_tray.lock() { g.dispatch_action("open_web_panel"); }
            }
            TrayCommand::ExitApp => {
                let _ = slint::invoke_from_event_loop(move || {
                    let _ = slint::quit_event_loop();
                });
            }
        })
        .ok()
    };

    if launch_minimized && tray_handle.is_some() {
        window_visible.store(false, Ordering::Relaxed);
        if let Ok(guard) = registry.lock() {
            guard.notify_visibility(false);
        }
    } else {
        show_and_configure_window(&app, &config, &registry, &window_visible, &last_hwnd, false);
    }

    {
        let reg_close = Arc::clone(&registry);
        let vis_close = Arc::clone(&window_visible);
        let cfg_close = Arc::clone(&config);
        let app_weak_close = app.as_weak();
        app.window().on_close_requested(move || {
            if let Some(a) = app_weak_close.upgrade() {
                if let Some(geom) = read_window_geometry(extract_hwnd(a.window())) {
                    if let Ok(mut g) = cfg_close.lock() {
                        g.core.window_geometry = geom;
                    }
                }
            }
            let to_tray = cfg_close.lock().map(|g| g.core.close_to_tray).unwrap_or(true);
            if to_tray {
                vis_close.store(false, Ordering::Relaxed);
                if let Ok(g) = reg_close.lock() { g.notify_visibility(false); }
                CloseRequestResponse::HideWindow
            } else {
                let _ = slint::quit_event_loop();
                CloseRequestResponse::HideWindow
            }
        });
    }

    // Initial diagnostics population
    {
        let (pt, c, r, w) = (portable.clone(), Arc::clone(&config), Arc::clone(&registry), app.as_weak());
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(800));
            ui_bridge::push_diagnostics_to_ui(&pt, &c, &r, &w);
        });
    }

    let _ = slint::run_event_loop_until_quit();

    // Clean shutdown: persist geometry, stop tray & modules, checkpoint WAL
    if let Some(geom) = read_window_geometry(extract_hwnd(app.window())) {
        if let Ok(mut g) = config.lock() {
            g.core.window_geometry = geom;
        }
    }
    if let Ok(g) = config.lock() {
        let _ = save_settings(&paths, &g);
    }
    if let Some(mut th) = tray_handle.take() {
        th.shutdown_handle();
    }
    if let Ok(mut guard) = registry.lock() {
        guard.shutdown_all();
    }
    if let Some(database) = db {
        database.checkpoint_and_close();
    }
    single_instance.release();
}
