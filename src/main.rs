#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod core;
mod modules;

use crate::core::diagnostics::{
    self, clear_logs, format_ai_markdown, format_plain_report, get_logs, run_diagnostics,
};
use crate::core::storage::{load_config, save_config, AppConfig, PortablePaths};
use crate::core::tray::{TrayCommand, TrayHandle};
use crate::modules::torrserver::TorrServerModule;
use crate::modules::ModuleRegistry;
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, VecModel};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

slint::include_modules!();

struct Win32Hooks {
    ui_weak: slint::Weak<AppWindow>,
    registry: Arc<Mutex<ModuleRegistry>>,
    window_visible: Arc<AtomicBool>,
}

static WIN32_HOOKS: OnceLock<Win32Hooks> = OnceLock::new();
static PREV_WNDPROC: AtomicIsize = AtomicIsize::new(0);
static SHOW_WINDOW_MSG: AtomicU32 = AtomicU32::new(0);

#[cfg(windows)]
fn acquire_single_instance_mutex() -> Option<windows_sys::Win32::Foundation::HANDLE> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, PostMessageW, RegisterWindowMessageW,
    };

    let mutex_name: Vec<u16> = "Local\\TorrServerUI_SingleInstance_Mutex\0"
        .encode_utf16()
        .collect();
    let msg_name: Vec<u16> = "TorrServerUI_ShowWindow\0".encode_utf16().collect();
    let title_w: Vec<u16> = "TorrServerUI\0".encode_utf16().collect();

    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr());
        let msg_id = RegisterWindowMessageW(msg_name.as_ptr());
        SHOW_WINDOW_MSG.store(msg_id, Ordering::SeqCst);

        if GetLastError() == ERROR_ALREADY_EXISTS {
            let existing_hwnd = FindWindowW(std::ptr::null(), title_w.as_ptr());
            if !existing_hwnd.is_null() && msg_id != 0 {
                PostMessageW(existing_hwnd, msg_id, 0, 0);
            }
            return None;
        }
        Some(handle)
    }
}

#[cfg(windows)]
fn extract_hwnd(window: &slint::Window) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    if let Ok(handle) = window.window_handle().window_handle() {
        if let RawWindowHandle::Win32(win32) = handle.as_raw() {
            return win32.hwnd.get();
        }
    }
    0
}

#[cfg(not(windows))]
fn extract_hwnd(_window: &slint::Window) -> isize {
    0
}

#[cfg(windows)]
unsafe extern "system" fn subclass_wndproc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, SetForegroundWindow, ShowWindow, SW_RESTORE, WM_DROPFILES, WNDPROC,
    };

    let show_msg = SHOW_WINDOW_MSG.load(Ordering::SeqCst);
    if show_msg != 0 && msg == show_msg {
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
        if let Some(hooks) = WIN32_HOOKS.get() {
            hooks.window_visible.store(true, Ordering::Relaxed);
            if let Ok(reg) = hooks.registry.lock() {
                reg.notify_visibility(true);
            }
            let weak = hooks.ui_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = weak.upgrade() {
                    let _ = app.show();
                }
            });
        }
        return 0;
    }

    if msg == WM_DROPFILES {
        let hdrop = wparam as HDROP;
        let count = DragQueryFileW(hdrop, u32::MAX, std::ptr::null_mut(), 0);
        let mut dropped_paths = Vec::new();
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0);
            if len > 0 {
                let mut buf = vec![0u16; (len + 1) as usize];
                DragQueryFileW(hdrop, i, buf.as_mut_ptr(), len + 1);
                if let Ok(s) = String::from_utf16(&buf[..len as usize]) {
                    dropped_paths.push(PathBuf::from(s));
                }
            }
        }
        DragFinish(hdrop);

        if !dropped_paths.is_empty() {
            if let Some(hooks) = WIN32_HOOKS.get() {
                if let Ok(reg) = hooks.registry.lock() {
                    reg.dispatch_dropped_files(&dropped_paths);
                }
            }
        }
        return 0;
    }

    let prev_ptr = PREV_WNDPROC.load(Ordering::SeqCst);
    let prev_proc: WNDPROC = std::mem::transmute(prev_ptr);
    CallWindowProcW(prev_proc, hwnd, msg, wparam, lparam)
}

#[cfg(windows)]
fn install_win32_subclass(hwnd_val: isize) {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::Shell::DragAcceptFiles;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SetWindowLongPtrW, GWLP_WNDPROC};

    if hwnd_val == 0 {
        return;
    }
    unsafe {
        let hwnd = hwnd_val as HWND;
        DragAcceptFiles(hwnd, 1);
        let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, subclass_wndproc as *const () as isize);
        PREV_WNDPROC.store(prev, Ordering::SeqCst);
    }
}

fn apply_theme_and_dwm(app: &AppWindow, mode: i32, hwnd: isize) {
    let sys_dark = core::detect_windows_dark_mode();
    let is_dark = mode == 2 || (mode == 0 && sys_dark);
    app.invoke_apply_theme(mode, sys_dark);
    core::apply_dwm_dark_mode(hwnd, is_dark);
}

fn push_diagnostics_to_ui(
    paths: &PortablePaths,
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

    let report = run_diagnostics(paths, &cfg, &snapshot);
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

fn main() {
    #[cfg(windows)]
    let _instance_mutex = match acquire_single_instance_mutex() {
        Some(h) => h,
        None => return,
    };

    let paths = match PortablePaths::init() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };

    diagnostics::info(
        "[CORE]",
        format!(
            "Запуск TorrServerUI v{} (портативный режим: {})",
            env!("CARGO_PKG_VERSION"),
            paths.exe_dir.display()
        ),
    );

    let loaded_cfg = load_config(&paths);
    let config = Arc::new(Mutex::new(loaded_cfg));

    // Initialize Module Registry and register Module #1: TorrServer
    let mut reg = ModuleRegistry::new(&paths.modules_dir);
    reg.register(Box::new(TorrServerModule::new(
        paths.clone(),
        Arc::clone(&config),
    )));
    reg.init_all();

    let registry = Arc::new(Mutex::new(reg));

    let app = AppWindow::new().expect("Не удалось создать AppWindow");
    let icon_rgba = core::generate_app_icon_rgba();
    app.set_app_icon(core::slint_app_icon(&icon_rgba));

    let init_theme = config.lock().map(|c| c.core.theme_mode).unwrap_or(0);
    app.set_theme_mode(init_theme);
    app.set_autostart_enabled(core::is_autostart_enabled());
    app.set_diag_app_version(env!("CARGO_PKG_VERSION").into());

    if let Ok(r) = registry.lock() {
        r.bind_all_ui(&app);
        app.set_registered_modules(ModelRc::from(Rc::new(VecModel::from(r.ui_module_list()))));
    }

    let window_visible = Arc::new(AtomicBool::new(true));
    let _ = WIN32_HOOKS.set(Win32Hooks {
        ui_weak: app.as_weak(),
        registry: Arc::clone(&registry),
        window_visible: Arc::clone(&window_visible),
    });

    // Show window first so Win32 HWND is guaranteed to exist
    app.show().expect("Не удалось отобразить главное окно");
    let hwnd = extract_hwnd(app.window());
    apply_theme_and_dwm(&app, init_theme, hwnd);

    #[cfg(windows)]
    install_win32_subclass(hwnd);

    // Spawn Tray Icon thread
    let mut tray_handle = {
        let ui_weak = app.as_weak();
        let reg_tray = Arc::clone(&registry);
        let vis_tray = Arc::clone(&window_visible);
        TrayHandle::spawn(icon_rgba, move |cmd| match cmd {
            TrayCommand::ToggleWindow => {
                let w = ui_weak.clone();
                let r = Arc::clone(&reg_tray);
                let v = Arc::clone(&vis_tray);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = w.upgrade() {
                        if v.load(Ordering::Relaxed) {
                            v.store(false, Ordering::Relaxed);
                            let _ = app.hide();
                            if let Ok(guard) = r.lock() {
                                guard.notify_visibility(false);
                            }
                        } else {
                            v.store(true, Ordering::Relaxed);
                            let _ = app.show();
                            if let Ok(guard) = r.lock() {
                                guard.notify_visibility(true);
                            }
                        }
                    }
                });
            }
            TrayCommand::ShowWindow => {
                let w = ui_weak.clone();
                let r = Arc::clone(&reg_tray);
                let v = Arc::clone(&vis_tray);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = w.upgrade() {
                        v.store(true, Ordering::Relaxed);
                        let _ = app.show();
                        if let Ok(guard) = r.lock() {
                            guard.notify_visibility(true);
                        }
                    }
                });
            }
            TrayCommand::RestartServer => {
                if let Ok(guard) = reg_tray.lock() {
                    guard.dispatch_action("restart_server");
                }
            }
            TrayCommand::OpenWebPanel => {
                if let Ok(guard) = reg_tray.lock() {
                    guard.dispatch_action("open_web_panel");
                }
            }
            TrayCommand::ExitApp => {
                let _ = slint::invoke_from_event_loop(move || {
                    let _ = slint::quit_event_loop();
                });
            }
        })
        .ok()
    };

    // Hide to tray on close button
    {
        let reg_close = Arc::clone(&registry);
        let vis_close = Arc::clone(&window_visible);
        app.window().on_close_requested(move || {
            vis_close.store(false, Ordering::Relaxed);
            if let Ok(guard) = reg_close.lock() {
                guard.notify_visibility(false);
            }
            CloseRequestResponse::HideWindow
        });
    }

    // Core UI Callbacks
    {
        let paths_c = paths.clone();
        let cfg_c = Arc::clone(&config);
        let weak = app.as_weak();
        app.on_change_theme_mode(move |mode| {
            if let Ok(mut guard) = cfg_c.lock() {
                guard.core.theme_mode = mode;
                let _ = save_config(&paths_c, &guard);
            }
            if let Some(app) = weak.upgrade() {
                let h = extract_hwnd(app.window());
                apply_theme_and_dwm(&app, mode, h);
            }
        });
    }

    {
        let paths_c = paths.clone();
        let cfg_c = Arc::clone(&config);
        let weak = app.as_weak();
        app.on_toggle_autostart(move |enabled| {
            let res = core::set_autostart(enabled, &paths_c.exe_path);
            if let Ok(mut guard) = cfg_c.lock() {
                guard.core.autostart = enabled;
                let _ = save_config(&paths_c, &guard);
            }
            if let Some(app) = weak.upgrade() {
                match res {
                    Ok(_) => core::show_toast(
                        &app,
                        if enabled {
                            "Автозапуск включён (ярлык создан)"
                        } else {
                            "Автозапуск отключён"
                        },
                    ),
                    Err(e) => {
                        app.set_autostart_enabled(!enabled);
                        core::show_toast(&app, e);
                    }
                }
            }
        });
    }

    {
        let p = paths.clone();
        let c = Arc::clone(&config);
        let r = Arc::clone(&registry);
        let w = app.as_weak();
        app.on_run_diagnostics_now(move || {
            let p2 = p.clone();
            let c2 = Arc::clone(&c);
            let r2 = Arc::clone(&r);
            let w2 = w.clone();
            thread::spawn(move || {
                diagnostics::info("[CORE]", "Запущена ручная диагностика");
                push_diagnostics_to_ui(&p2, &c2, &r2, &w2);
            });
        });
    }

    {
        let p = paths.clone();
        let c = Arc::clone(&config);
        let r = Arc::clone(&registry);
        let w = app.as_weak();
        app.on_copy_diagnostics_report(move || {
            let p2 = p.clone();
            let c2 = Arc::clone(&c);
            let r2 = Arc::clone(&r);
            let w2 = w.clone();
            thread::spawn(move || {
                let cfg = c2.lock().map(|g| g.clone()).unwrap_or_default();
                let snap = r2
                    .lock()
                    .map(|reg| reg.diagnostics_snapshot())
                    .unwrap_or_default();
                let rep = run_diagnostics(&p2, &cfg, &snap);
                let text = format_plain_report(&rep);
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(text);
                }
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = w2.upgrade() {
                        core::show_toast(&app, "Диагностический отчёт скопирован");
                    }
                });
            });
        });
    }

    {
        let p = paths.clone();
        let c = Arc::clone(&config);
        let r = Arc::clone(&registry);
        let w = app.as_weak();
        app.on_copy_diagnostics_for_ai(move || {
            let p2 = p.clone();
            let c2 = Arc::clone(&c);
            let r2 = Arc::clone(&r);
            let w2 = w.clone();
            thread::spawn(move || {
                let cfg = c2.lock().map(|g| g.clone()).unwrap_or_default();
                let snap = r2
                    .lock()
                    .map(|reg| reg.diagnostics_snapshot())
                    .unwrap_or_default();
                let rep = run_diagnostics(&p2, &cfg, &snap);
                let md = format_ai_markdown(&rep);
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(md);
                }
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = w2.upgrade() {
                        core::show_toast(&app, "Markdown-отчёт для ИИ скопирован в буфер");
                    }
                });
            });
        });
    }

    {
        let p = paths.clone();
        let c = Arc::clone(&config);
        let r = Arc::clone(&registry);
        let w = app.as_weak();
        app.on_clear_diagnostics_logs(move || {
            clear_logs();
            push_diagnostics_to_ui(&p, &c, &r, &w);
        });
    }

    {
        let p = paths.clone();
        let c = Arc::clone(&config);
        let r = Arc::clone(&registry);
        let w = app.as_weak();
        app.on_open_diagnostics_tab(move || {
            if let Some(app) = w.upgrade() {
                app.set_active_tab(3);
                app.set_diag_expander_open(true);
            }
            let p2 = p.clone();
            let c2 = Arc::clone(&c);
            let r2 = Arc::clone(&r);
            let w2 = w.clone();
            thread::spawn(move || {
                push_diagnostics_to_ui(&p2, &c2, &r2, &w2);
            });
        });
    }

    // Live diagnostics refresh when Settings tab + Diagnostics expander is open
    {
        let p = paths.clone();
        let c = Arc::clone(&config);
        let r = Arc::clone(&registry);
        let w = app.as_weak();
        let vis = Arc::clone(&window_visible);
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, Duration::from_secs(2), move || {
            if !vis.load(Ordering::Relaxed) {
                return;
            }
            if let Some(app) = w.upgrade() {
                if app.get_active_tab() == 3 && app.get_diag_expander_open() {
                    let p2 = p.clone();
                    let c2 = Arc::clone(&c);
                    let r2 = Arc::clone(&r);
                    let w2 = w.clone();
                    thread::spawn(move || {
                        push_diagnostics_to_ui(&p2, &c2, &r2, &w2);
                    });
                }
            }
        });
        std::mem::forget(timer);
    }

    // Initial diagnostics population
    {
        let p = paths.clone();
        let c = Arc::clone(&config);
        let r = Arc::clone(&registry);
        let w = app.as_weak();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(900));
            push_diagnostics_to_ui(&p, &c, &r, &w);
        });
    }

    let _ = slint::run_event_loop_until_quit();

    // Clean shutdown
    if let Some(mut th) = tray_handle.take() {
        th.shutdown();
    }
    if let Ok(mut guard) = registry.lock() {
        guard.shutdown_all();
    }

    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::Foundation::CloseHandle(_instance_mutex);
    }
}
