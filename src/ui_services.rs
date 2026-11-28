use crate::core::config::{save_settings, AppConfig, WorkspacePreset};
use crate::core::diagnostics::{
    self, clear_logs, format_ai_markdown, format_plain_report, run_diagnostics,
};
use crate::core::paths::AppPaths;
use crate::core::secrets::{FileSecretStore, SecretStore};
use crate::core::storage::PortablePaths;
use crate::core::sync::{ConflictResolution, SyncOutcome, SyncProvider, WebDavSyncProvider};
use crate::modules::ModuleRegistry;
use crate::platform::windows::metrics::WindowsProcessMetrics;
use crate::platform::windows::open_url_in_browser;
use crate::platform::ProcessMetrics;
use crate::services::updater;
use crate::ui_bridge::{push_diagnostics_to_ui, push_workspace_to_ui, show_toast};
use crate::{AppState, AppWindow};
use slint::ComponentHandle;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub fn bind_services_and_workspace(
    app: &AppWindow,
    paths: &AppPaths,
    portable: &PortablePaths,
    config: &Arc<Mutex<AppConfig>>,
    registry: &Arc<Mutex<ModuleRegistry>>,
    secrets: &Arc<Mutex<FileSecretStore>>,
) {
    bind_webdav_and_profiler(app, paths, portable, config, registry, secrets);
    bind_workspace_and_about(app, paths, config);
}

fn bind_webdav_and_profiler(
    app: &AppWindow,
    paths: &AppPaths,
    portable: &PortablePaths,
    config: &Arc<Mutex<AppConfig>>,
    registry: &Arc<Mutex<ModuleRegistry>>,
    secrets: &Arc<Mutex<FileSecretStore>>,
) {
    // Save WebDAV & SecretStore settings
    {
        let p = paths.clone();
        let cfg = Arc::clone(config);
        let sec = Arc::clone(secrets);
        let weak = app.as_weak();
        app.on_save_webdav_settings(move || {
            let Some(a) = weak.upgrade() else { return };
            let enabled = a.get_webdav_enabled();
            let preset = a.get_webdav_preset();
            let url = a.get_webdav_url().to_string();
            let user = a.get_webdav_user().to_string();
            let pass = a.get_webdav_pass().to_string();
            let sync_db = a.get_webdav_sync_db();
            let secret_mode = a.get_secret_mode();
            let master_pw = a.get_master_password().to_string();

            if let Ok(mut g) = cfg.lock() {
                g.core.webdav.enabled = enabled;
                g.core.webdav.preset = preset;
                g.core.webdav.url = url;
                g.core.webdav.username = user;
                g.core.webdav.sync_db = sync_db;
                g.core.webdav.secret_mode = secret_mode;
                let _ = save_settings(&p, &g);
            }
            if let Ok(mut s) = sec.lock() {
                s.set_master_password(if secret_mode == 1 && !master_pw.is_empty() {
                    Some(master_pw)
                } else {
                    None
                });
                let _ = s.set_secret("webdav_pass", &pass);
            }
            a.global::<AppState>().set_sync_status_text(
                if enabled {
                    "WebDAV: вкл."
                } else {
                    "Синхр.: выкл."
                }
                .into(),
            );
            show_toast(&a, "Параметры WebDAV и SecretStore сохранены");
        });
    }

    // Test & Sync WebDAV
    {
        let weak = app.as_weak();
        app.on_test_webdav_connection(move || {
            let Some(a) = weak.upgrade() else { return };
            let url = a.get_webdav_url().to_string();
            let user = a.get_webdav_user().to_string();
            let pass = a.get_webdav_pass().to_string();
            let w = weak.clone();
            thread::spawn(move || {
                let msg = WebDavSyncProvider::new(&url, &user, &pass)
                    .and_then(|prov| prov.test_connection())
                    .unwrap_or_else(|e| e.to_string());
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = w.upgrade() {
                        show_toast(&app, msg);
                    }
                });
            });
        });
    }
    {
        let p = paths.clone();
        let cfg = Arc::clone(config);
        let weak = app.as_weak();
        app.on_sync_webdav_now(move || {
            let Some(a) = weak.upgrade() else { return };
            let url = a.get_webdav_url().to_string();
            let user = a.get_webdav_user().to_string();
            let pass = a.get_webdav_pass().to_string();
            let sync_db = a.get_webdav_sync_db();
            let local_snap = cfg.lock().map(|g| g.clone()).unwrap_or_default();
            let w = weak.clone();
            let p2 = p.clone();
            let cfg2 = Arc::clone(&cfg);
            a.global::<AppState>()
                .set_sync_status_text("Синхронизация...".into());
            thread::spawn(move || {
                let res = WebDavSyncProvider::new(&url, &user, &pass)
                    .and_then(|prov| prov.sync_now(&p2, &local_snap, sync_db));
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(app) = w.upgrade() else { return };
                    match res {
                        Ok(SyncOutcome::Synced(merged, msg)) => {
                            if let Ok(mut g) = cfg2.lock() {
                                *g = merged.clone();
                                let _ = save_settings(&p2, &g);
                            }
                            push_workspace_to_ui(&app, &merged);
                            app.global::<AppState>()
                                .set_sync_status_text("WebDAV: синхр. OK".into());
                            show_toast(&app, msg);
                        }
                        Ok(SyncOutcome::ConflictPrompt {
                            merged_config,
                            summary,
                        }) => {
                            if let Ok(mut g) = cfg2.lock() {
                                *g = merged_config;
                                let _ = save_settings(&p2, &g);
                            }
                            app.set_sync_conflict_info(summary.into());
                            app.set_sync_conflict_visible(true);
                        }
                        Err(e) => {
                            app.global::<AppState>()
                                .set_sync_status_text("WebDAV: ошибка".into());
                            show_toast(&app, e.to_string());
                        }
                    }
                });
            });
        });
    }
    {
        let p = paths.clone();
        let weak = app.as_weak();
        app.on_resolve_sync_conflict(move |choice| {
            let Some(a) = weak.upgrade() else { return };
            let url = a.get_webdav_url().to_string();
            let user = a.get_webdav_user().to_string();
            let pass = a.get_webdav_pass().to_string();
            let res_mode = if choice == 1 {
                ConflictResolution::UseRemote
            } else {
                ConflictResolution::KeepLocal
            };
            let w = weak.clone();
            let p2 = p.clone();
            thread::spawn(move || {
                let msg = WebDavSyncProvider::new(&url, &user, &pass)
                    .and_then(|prov| prov.resolve_db_conflict(&p2, res_mode))
                    .unwrap_or_else(|e| e.to_string());
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = w.upgrade() {
                        app.global::<AppState>()
                            .set_sync_status_text("WebDAV: синхр. OK".into());
                        show_toast(&app, msg);
                    }
                });
            });
        });
    }

    // Live Profiler
    let profiler = Arc::new(Mutex::new(WindowsProcessMetrics::new()));
    {
        let p = paths.clone();
        let cfg = Arc::clone(config);
        app.on_toggle_profiler(move |val| {
            if let Ok(mut g) = cfg.lock() {
                g.core.profiler_enabled = val;
                let _ = save_settings(&p, &g);
            }
        });
    }
    {
        let p = paths.clone();
        let prof = Arc::clone(&profiler);
        let weak = app.as_weak();
        app.on_export_profiler_report(move || {
            let snap = prof.lock().map(|mut m| m.sample()).unwrap_or_default();
            let out = p
                .exports_dir
                .join(format!("profiler_{}.txt", chrono::Utc::now().timestamp()));
            let body = format!(
                "TorrServerUI Profiler Report\nCPU: {:.1}%\nWorking Set: {:.1} MB\nPrivate Bytes: {:.1} MB\nHandles: {}\nGDI: {}\nUSER: {}\n",
                snap.cpu_percent,
                snap.working_set_bytes as f64 / 1_048_576.0,
                snap.private_bytes as f64 / 1_048_576.0,
                snap.handle_count,
                snap.gdi_objects,
                snap.user_objects
            );
            let _ = std::fs::write(&out, body);
            if let Some(a) = weak.upgrade() {
                show_toast(&a, format!("Отчёт экспортирован: {}", out.display()));
            }
        });
    }
    {
        let w = app.as_weak();
        let prof = Arc::clone(&profiler);
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, Duration::from_secs(2), move || {
            let Some(a) = w.upgrade() else { return };
            let snap = prof.lock().map(|mut m| m.sample()).unwrap_or_default();
            let ws_mb = snap.working_set_bytes as f64 / 1_048_576.0;
            let state = a.global::<AppState>();
            state.set_telemetry_cpu(format!("{:.0}%", snap.cpu_percent).into());
            state.set_telemetry_ram(format!("{:.0} МБ", ws_mb).into());
            if a.get_profiler_enabled() {
                a.set_profiler_cpu(format!("{:.1}%", snap.cpu_percent).into());
                a.set_profiler_working_set(format!("{:.1} МБ", ws_mb).into());
                a.set_profiler_private_bytes(
                    format!("{:.1} МБ", snap.private_bytes as f64 / 1_048_576.0).into(),
                );
                a.set_profiler_handles(snap.handle_count.to_string().into());
                a.set_profiler_gdi_user(format!("{} / {}", snap.gdi_objects, snap.user_objects).into());
            }
        });
        std::mem::forget(timer);
    }

    // Diagnostics callbacks
    {
        let pt = portable.clone();
        let c = Arc::clone(config);
        let r = Arc::clone(registry);
        let w = app.as_weak();
        app.on_run_diagnostics_now(move || {
            let (pt2, c2, r2, w2) = (pt.clone(), Arc::clone(&c), Arc::clone(&r), w.clone());
            thread::spawn(move || {
                diagnostics::info("[CORE]", "Запущена ручная диагностика");
                push_diagnostics_to_ui(&pt2, &c2, &r2, &w2);
            });
        });
    }
    {
        let pt = portable.clone();
        let c = Arc::clone(config);
        let r = Arc::clone(registry);
        let w = app.as_weak();
        app.on_copy_diagnostics_report(move || {
            let (pt2, c2, r2, w2) = (pt.clone(), Arc::clone(&c), Arc::clone(&r), w.clone());
            thread::spawn(move || {
                let cfg = c2.lock().map(|g| g.clone()).unwrap_or_default();
                let snap = r2
                    .lock()
                    .map(|reg| reg.diagnostics_snapshot())
                    .unwrap_or_default();
                let text = format_plain_report(&run_diagnostics(&pt2, &cfg, &snap));
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(text);
                }
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(a) = w2.upgrade() {
                        show_toast(&a, "Отчёт скопирован");
                    }
                });
            });
        });
    }
    {
        let pt = portable.clone();
        let c = Arc::clone(config);
        let r = Arc::clone(registry);
        let w = app.as_weak();
        app.on_copy_diagnostics_for_ai(move || {
            let (pt2, c2, r2, w2) = (pt.clone(), Arc::clone(&c), Arc::clone(&r), w.clone());
            thread::spawn(move || {
                let cfg = c2.lock().map(|g| g.clone()).unwrap_or_default();
                let snap = r2
                    .lock()
                    .map(|reg| reg.diagnostics_snapshot())
                    .unwrap_or_default();
                let md = format_ai_markdown(&run_diagnostics(&pt2, &cfg, &snap));
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(md);
                }
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(a) = w2.upgrade() {
                        show_toast(&a, "Markdown для ИИ скопирован");
                    }
                });
            });
        });
    }
    {
        let pt = portable.clone();
        let c = Arc::clone(config);
        let r = Arc::clone(registry);
        let w = app.as_weak();
        app.on_clear_diagnostics_logs(move || {
            clear_logs();
            push_diagnostics_to_ui(&pt, &c, &r, &w);
        });
    }
    {
        let pt = portable.clone();
        let c = Arc::clone(config);
        let r = Arc::clone(registry);
        let w = app.as_weak();
        app.on_open_diagnostics_tab(move || {
            if let Some(a) = w.upgrade() {
                a.set_active_tab(3);
                a.global::<AppState>().set_mobile_detail_open(true);
            }
            let (pt2, c2, r2, w2) = (pt.clone(), Arc::clone(&c), Arc::clone(&r), w.clone());
            thread::spawn(move || push_diagnostics_to_ui(&pt2, &c2, &r2, &w2));
        });
    }
}

fn bind_workspace_and_about(app: &AppWindow, paths: &AppPaths, config: &Arc<Mutex<AppConfig>>) {
    {
        let p = paths.clone();
        let cfg = Arc::clone(config);
        let weak = app.as_weak();
        app.on_ws_load_preset(move |idx| {
            let Ok(mut g) = cfg.lock() else { return };
            if let Some(name) = g
                .core
                .workspace_presets
                .get(idx.max(0) as usize)
                .map(|pr| pr.name.clone())
            {
                g.core.active_workspace_preset = name;
                let _ = save_settings(&p, &g);
                if let Some(a) = weak.upgrade() {
                    push_workspace_to_ui(&a, &g);
                }
            }
        });
    }
    {
        let p = paths.clone();
        let cfg = Arc::clone(config);
        let weak = app.as_weak();
        app.on_ws_save_preset(move |name| {
            let clean = name.trim().to_string();
            if clean.is_empty() {
                return;
            }
            let Ok(mut g) = cfg.lock() else { return };
            let cur_blocks = g
                .core
                .workspace_presets
                .iter()
                .find(|pr| pr.name == g.core.active_workspace_preset)
                .map(|pr| pr.blocks.clone())
                .unwrap_or_else(|| WorkspacePreset::default().blocks);
            let now = chrono::Utc::now().timestamp();
            if let Some(ex) = g.core.workspace_presets.iter_mut().find(|pr| pr.name == clean) {
                ex.blocks = cur_blocks;
                ex.updated_at = now;
            } else {
                g.core.workspace_presets.push(WorkspacePreset {
                    name: clean.clone(),
                    blocks: cur_blocks,
                    updated_at: now,
                });
            }
            g.core.active_workspace_preset = clean;
            let _ = save_settings(&p, &g);
            if let Some(a) = weak.upgrade() {
                push_workspace_to_ui(&a, &g);
                show_toast(&a, "Пресет рабочего пространства сохранён");
            }
        });
    }
    {
        let p = paths.clone();
        let cfg = Arc::clone(config);
        let weak = app.as_weak();
        app.on_ws_delete_preset(move |idx| {
            let Ok(mut g) = cfg.lock() else { return };
            if g.core.workspace_presets.len() <= 1 {
                return;
            }
            let i = idx.max(0) as usize;
            if i < g.core.workspace_presets.len() {
                g.core.workspace_presets.remove(i);
                g.core.active_workspace_preset = g.core.workspace_presets[0].name.clone();
                let _ = save_settings(&p, &g);
                if let Some(a) = weak.upgrade() {
                    push_workspace_to_ui(&a, &g);
                }
            }
        });
    }
    {
        let p = paths.clone();
        let cfg = Arc::clone(config);
        let weak = app.as_weak();
        app.on_ws_set_block_span(move |id, span| {
            let Ok(mut g) = cfg.lock() else { return };
            let active = g.core.active_workspace_preset.clone();
            if let Some(pr) = g.core.workspace_presets.iter_mut().find(|x| x.name == active) {
                if let Some(b) = pr.blocks.iter_mut().find(|b| b.id == id.as_str()) {
                    b.span = span.clamp(1, 3);
                    pr.updated_at = chrono::Utc::now().timestamp();
                }
            }
            let _ = save_settings(&p, &g);
            if let Some(a) = weak.upgrade() {
                push_workspace_to_ui(&a, &g);
            }
        });
    }
    {
        let p = paths.clone();
        let cfg = Arc::clone(config);
        let weak = app.as_weak();
        app.on_ws_move_block(move |id, dir| {
            let Ok(mut g) = cfg.lock() else { return };
            let active = g.core.active_workspace_preset.clone();
            if let Some(pr) = g.core.workspace_presets.iter_mut().find(|x| x.name == active) {
                pr.blocks.sort_by_key(|b| b.order);
                if let Some(pos) = pr.blocks.iter().position(|b| b.id == id.as_str()) {
                    let new_pos = (pos as i32 + dir).clamp(0, pr.blocks.len() as i32 - 1) as usize;
                    pr.blocks.swap(pos, new_pos);
                    for (idx, blk) in pr.blocks.iter_mut().enumerate() {
                        blk.order = idx as i32;
                    }
                    pr.updated_at = chrono::Utc::now().timestamp();
                }
            }
            let _ = save_settings(&p, &g);
            if let Some(a) = weak.upgrade() {
                push_workspace_to_ui(&a, &g);
            }
        });
    }
    {
        let weak = app.as_weak();
        app.on_check_github_updates(move || {
            let w = weak.clone();
            if let Some(a) = w.upgrade() {
                a.set_update_check_status("Проверка GitHub Releases...".into());
            }
            thread::spawn(move || {
                let res = updater::check_github_release(
                    "torrserver-ui/TorrServerUI",
                    env!("CARGO_PKG_VERSION"),
                );
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(a) = w.upgrade() {
                        match res {
                            Ok(info) => {
                                a.set_update_check_status(info.message.into());
                                if info.has_update {
                                    open_url_in_browser(&info.release_url);
                                }
                            }
                            Err(e) => a.set_update_check_status(e.into()),
                        }
                    }
                });
            });
        });
    }
}
