pub mod api;
pub mod clipboard;
pub mod player;
pub mod rutracker;
pub mod server_manager;

use crate::core;
use crate::core::diagnostics;
use crate::core::storage::{save_config, AppConfig, PortablePaths, TorrServerConfig};
use crate::modules::torrserver::api::{
    format_bytes, format_speed, parse_backup_json, ApiSearchResult, ApiTorrent, TorrServerApi,
    TorrServerEngineSettings,
};
use crate::modules::torrserver::clipboard::ClipboardWatcher;
use crate::modules::torrserver::player::{
    detect_installed_players, is_video_filename, launch_player, load_history, resolve_player_exe,
    save_history, upsert_history_watch, write_m3u_playlist, DetectedPlayer, HistoryRecord,
};
use crate::modules::torrserver::rutracker::{RuTrackerClient, RuTrackerPayload};
use crate::modules::torrserver::server_manager::ServerManager;
use crate::modules::{AppModule, ModuleInfo, ModuleStatus};
use crate::{AppWindow, HistoryItem, SearchResultItem, TorrentFileItem, TorrentItem};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

struct TorrServerShared {
    paths: PortablePaths,
    config: Arc<Mutex<AppConfig>>,
    server: ServerManager,
    ui_weak: Mutex<Option<slint::Weak<AppWindow>>>,
    window_visible: Arc<AtomicBool>,
    poll_stop: Arc<AtomicBool>,
    cached_torrents: Mutex<Vec<ApiTorrent>>,
    selected_hash: Mutex<String>,
    search_raw: Mutex<Vec<ApiSearchResult>>,
    search_quality: Mutex<String>,
    search_sort: Mutex<i32>,
    search_source: Mutex<i32>,
    rutracker: RuTrackerClient,
    detected_players: Mutex<Vec<DetectedPlayer>>,
    history: Mutex<Vec<HistoryRecord>>,
    last_status: Mutex<ModuleStatus>,
    clipboard: Mutex<Option<ClipboardWatcher>>,
}

impl TorrServerShared {
    fn ts_config(&self) -> TorrServerConfig {
        self.config
            .lock()
            .map(|c| c.torrserver.clone())
            .unwrap_or_default()
    }

    fn update_ts_config<F: FnOnce(&mut TorrServerConfig)>(&self, f: F) {
        if let Ok(mut cfg) = self.config.lock() {
            f(&mut cfg.torrserver);
            if let Err(e) = save_config(&self.paths, &cfg) {
                diagnostics::error("[TS:API]", e);
            }
        }
    }

    fn toast(&self, msg: impl Into<String>) {
        let msg = msg.into();
        if let Ok(guard) = self.ui_weak.lock() {
            if let Some(weak) = guard.as_ref() {
                let weak_c = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = weak_c.upgrade() {
                        core::show_toast(&app, msg);
                    }
                });
            }
        }
    }
}

pub struct TorrServerModule {
    shared: Arc<TorrServerShared>,
}

impl TorrServerModule {
    pub fn new(paths: PortablePaths, config: Arc<Mutex<AppConfig>>) -> Self {
        let history = load_history(&paths.ts_history_path);
        let players = detect_installed_players();
        let server = ServerManager::new(paths.clone());

        let shared = Arc::new(TorrServerShared {
            paths,
            config,
            server,
            ui_weak: Mutex::new(None),
            window_visible: Arc::new(AtomicBool::new(true)),
            poll_stop: Arc::new(AtomicBool::new(false)),
            cached_torrents: Mutex::new(Vec::new()),
            selected_hash: Mutex::new(String::new()),
            search_raw: Mutex::new(Vec::new()),
            search_quality: Mutex::new("ALL".to_string()),
            search_sort: Mutex::new(0),
            search_source: Mutex::new(0),
            rutracker: RuTrackerClient::new(),
            detected_players: Mutex::new(players),
            history: Mutex::new(history),
            last_status: Mutex::new(ModuleStatus::default()),
            clipboard: Mutex::new(None),
        });

        Self { shared }
    }
}

impl AppModule for TorrServerModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "torrserver".to_string(),
            name: "TorrServer".to_string(),
            version: "1.0.0".to_string(),
            description: "Локальный/удалённый движок TorrServer, поиск, плееры и история".to_string(),
            enabled: true,
        }
    }

    fn on_init(&mut self) {
        let cfg = self.shared.ts_config();
        if cfg.mode == 0 {
            if self.shared.server.binary_exists() {
                if let Err(e) = self.shared.server.start(&cfg) {
                    diagnostics::error("[TS:ENGINE]", e);
                }
            } else {
                diagnostics::warn(
                    "[TS:ENGINE]",
                    "TorrServer-windows-amd64.exe отсутствует в data/torrserver/ — ожидается загрузка пользователем",
                );
            }
        }

        // Start clipboard watcher
        let shared_cb = Arc::clone(&self.shared);
        let watcher = ClipboardWatcher::start(cfg.clipboard_watch, move |magnet| {
            let shared_add = Arc::clone(&shared_cb);
            thread::spawn(move || {
                let api = TorrServerApi::from_config(&shared_add.ts_config());
                match api.add_magnet(&magnet, None) {
                    Ok(t) => {
                        let title = t.display_title();
                        shared_add.toast(format!("Добавлено из буфера: {title}"));
                        refresh_torrents_and_status(&shared_add);
                    }
                    Err(e) => {
                        diagnostics::warn("[TS:API]", format!("Не удалось добавить magnet из буфера: {e}"));
                    }
                }
            });
        });
        if let Ok(mut guard) = self.shared.clipboard.lock() {
            *guard = Some(watcher);
        }

        // Start background UI polling thread (pauses automatically when window hidden in tray)
        let shared_poll = Arc::clone(&self.shared);
        thread::Builder::new()
            .name("ts-ui-poller".to_string())
            .spawn(move || {
                // Give engine a brief moment to boot on startup
                thread::sleep(Duration::from_millis(700));
                sync_engine_settings_from_server(&shared_poll);
                while !shared_poll.poll_stop.load(Ordering::Relaxed) {
                    if shared_poll.window_visible.load(Ordering::Relaxed) {
                        refresh_torrents_and_status(&shared_poll);
                        thread::sleep(Duration::from_millis(1600));
                    } else {
                        thread::sleep(Duration::from_millis(500));
                    }
                }
            })
            .ok();
    }

    fn bind_ui(&self, ui: &AppWindow) {
        if let Ok(mut guard) = self.shared.ui_weak.lock() {
            *guard = Some(ui.as_weak());
        }

        // Initialize UI fields from config
        let cfg = self.shared.ts_config();
        ui.set_ts_mode(cfg.mode);
        ui.set_is_remote_mode(cfg.mode == 1);
        ui.set_ts_port(cfg.port.to_string().into());
        ui.set_ts_remote_url(cfg.remote_url.clone().into());
        ui.set_ts_username(cfg.username.clone().into());
        ui.set_ts_password(cfg.password.clone().into());
        ui.set_only_video_files(cfg.only_video_files);
        ui.set_engine_missing(!self.shared.server.binary_exists());

        ui.set_ts_cache_size_mb(cfg.cache_size_mb.to_string().into());
        ui.set_ts_preload_percent(cfg.preload_percent.to_string().into());
        ui.set_ts_read_ahead_percent(cfg.read_ahead_percent.to_string().into());
        ui.set_ts_connections_limit(cfg.connections_limit.to_string().into());
        ui.set_ts_use_disk(cfg.use_disk);
        ui.set_ts_enable_ipv6(cfg.enable_ipv6);
        ui.set_ts_enable_dlna(cfg.enable_dlna);
        ui.set_ts_enable_rutor(cfg.enable_rutor);

        ui.set_player_custom_path(cfg.player_custom_path.clone().into());
        ui.set_player_args(cfg.player_args.clone().into());
        ui.set_clipboard_watch(cfg.clipboard_watch);

        ui.set_rt_enabled(cfg.rutracker_enabled);
        ui.set_rt_mirror(cfg.rutracker_mirror.clone().into());
        ui.set_rt_username(cfg.rutracker_username.clone().into());
        ui.set_rt_password(cfg.rutracker_password.clone().into());
        ui.set_rt_proxy(cfg.rutracker_proxy.clone().into());
        ui.set_rt_cookie(cfg.rutracker_cookie.clone().into());
        if cfg.rutracker_enabled && cfg.has_rutracker_credentials() {
            ui.set_rt_auth_status("Учётные данные сохранены".into());
        }

        sync_players_to_ui(&self.shared, ui);
        sync_history_to_ui(&self.shared, ui);

        // 1. Add Magnet
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_add_magnet(move |magnet| {
                let link = magnet.trim().to_string();
                if link.is_empty() {
                    return;
                }
                if let Some(app) = weak.upgrade() {
                    app.set_magnet_input("".into());
                }
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    add_and_optionally_play(&s, &link, None, false);
                });
            });
        }

        // 2. Paste & Add Magnet
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_paste_and_add_magnet(move || {
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    if let Ok(text) = cb.get_text() {
                        let trimmed = text.trim().to_string();
                        if !trimmed.is_empty() {
                            if let Some(app) = weak.upgrade() {
                                app.set_magnet_input(trimmed.clone().into());
                            }
                            let s = Arc::clone(&shared);
                            thread::spawn(move || {
                                add_and_optionally_play(&s, &trimmed, None, false);
                            });
                            return;
                        }
                    }
                }
                shared.toast("Буфер обмена не содержит текста");
            });
        }

        // 3. Pick .torrent file
        {
            let shared = Arc::clone(&self.shared);
            ui.on_pick_torrent_file(move || {
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Torrent файлы", &["torrent"])
                        .pick_file()
                    {
                        upload_torrent_files(&s, &[path]);
                    }
                });
            });
        }

        // 4. Select torrent
        {
            let shared = Arc::clone(&self.shared);
            ui.on_select_torrent(move |hash| {
                if let Ok(mut sel) = shared.selected_hash.lock() {
                    *sel = hash.to_string();
                }
                push_torrents_to_ui(&shared);
            });
        }

        // 5. Refresh torrents manually
        {
            let shared = Arc::clone(&self.shared);
            ui.on_refresh_torrents(move || {
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    refresh_torrents_and_status(&s);
                });
            });
        }

        // 6. Drop torrent cache
        {
            let shared = Arc::clone(&self.shared);
            ui.on_drop_torrent(move |hash| {
                let h = hash.to_string();
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    let api = TorrServerApi::from_config(&s.ts_config());
                    match api.drop_torrent(&h) {
                        Ok(_) => {
                            diagnostics::info("[TS:API]", format!("Кеш сброшен (drop): {h}"));
                            s.toast("Кеш раздачи сброшен");
                            refresh_torrents_and_status(&s);
                        }
                        Err(e) => {
                            diagnostics::error("[TS:API]", e.clone());
                            s.toast(e);
                        }
                    }
                });
            });
        }

        // 7. Remove torrent
        {
            let shared = Arc::clone(&self.shared);
            ui.on_remove_torrent(move |hash| {
                let h = hash.to_string();
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    let api = TorrServerApi::from_config(&s.ts_config());
                    match api.remove_torrent(&h) {
                        Ok(_) => {
                            diagnostics::info("[TS:API]", format!("Раздача удалена: {h}"));
                            if let Ok(mut sel) = s.selected_hash.lock() {
                                if *sel == h {
                                    sel.clear();
                                }
                            }
                            s.toast("Раздача удалена");
                            refresh_torrents_and_status(&s);
                        }
                        Err(e) => {
                            diagnostics::error("[TS:API]", e.clone());
                            s.toast(e);
                        }
                    }
                });
            });
        }

        // 8. Copy magnet
        {
            let shared = Arc::clone(&self.shared);
            ui.on_copy_torrent_magnet(move |hash| {
                let h = hash.to_string();
                let magnet = format!("magnet:?xt=urn:btih:{h}");
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(magnet);
                    shared.toast("Magnet-ссылка скопирована");
                }
            });
        }

        // 9. Play file (file_id == -1 plays first video file)
        {
            let shared = Arc::clone(&self.shared);
            ui.on_play_file(move |hash, file_id| {
                let h = hash.to_string();
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    play_torrent_file(&s, &h, file_id);
                });
            });
        }

        // 10. Play all as .m3u
        {
            let shared = Arc::clone(&self.shared);
            ui.on_play_all_m3u(move |hash| {
                let h = hash.to_string();
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    play_torrent_m3u(&s, &h);
                });
            });
        }

        // 11. Copy direct stream URL
        {
            let shared = Arc::clone(&self.shared);
            ui.on_copy_stream_url(move |hash, file_id| {
                let h = hash.to_string();
                let cfg = shared.ts_config();
                let api = TorrServerApi::from_config(&cfg);
                let file_name = shared
                    .cached_torrents
                    .lock()
                    .ok()
                    .and_then(|list| {
                        list.iter()
                            .find(|t| t.hash == h)
                            .and_then(|t| {
                                t.files()
                                    .into_iter()
                                    .find(|f| f.id == file_id)
                                    .map(|f| f.path)
                            })
                    })
                    .unwrap_or_else(|| "video.mkv".to_string());
                let base_name = std::path::Path::new(&file_name)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("video.mkv");
                let url = api.stream_url(&h, file_id, base_name);
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(url);
                    shared.toast("Прямая ссылка потока скопирована");
                }
            });
        }

        // 12. Toggle Only Video files
        {
            let shared = Arc::clone(&self.shared);
            ui.on_toggle_only_video(move |only| {
                shared.update_ts_config(|c| c.only_video_files = only);
                push_torrents_to_ui(&shared);
            });
        }

        // 12a. Export torrents to .json backup
        {
            let shared = Arc::clone(&self.shared);
            ui.on_export_torrents(move || {
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    let api = TorrServerApi::from_config(&s.ts_config());
                    match api.export_torrents_json() {
                        Ok((_, 0)) => {
                            s.toast("Список раздач пуст — нечего экспортировать");
                        }
                        Ok((json, count)) => {
                            if let Some(path) = rfd::FileDialog::new()
                                .set_file_name("torrents_backup.json")
                                .add_filter("JSON бэкап TorrServer", &["json"])
                                .save_file()
                            {
                                match std::fs::write(&path, json) {
                                    Ok(_) => {
                                        diagnostics::info(
                                            "[TS:API]",
                                            format!(
                                                "Экспортировано раздач ({count}) в {}",
                                                path.display()
                                            ),
                                        );
                                        s.toast(format!("Экспортировано раздач: {count}"));
                                    }
                                    Err(e) => {
                                        let msg = format!("Ошибка сохранения файла: {e}");
                                        diagnostics::error("[TS:API]", msg.clone());
                                        s.toast(msg);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            diagnostics::error("[TS:API]", e.clone());
                            s.toast(e);
                        }
                    }
                });
            });
        }

        // 12b. Import torrents from .json backup
        {
            let shared = Arc::clone(&self.shared);
            ui.on_import_torrents(move || {
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    let Some(path) = rfd::FileDialog::new()
                        .add_filter("JSON бэкап TorrServer", &["json"])
                        .pick_file()
                    else {
                        return;
                    };

                    let raw = match std::fs::read_to_string(&path) {
                        Ok(txt) => txt,
                        Err(e) => {
                            let msg = format!("Ошибка чтения файла: {e}");
                            diagnostics::error("[TS:API]", msg.clone());
                            s.toast(msg);
                            return;
                        }
                    };

                    let items = match parse_backup_json(&raw) {
                        Ok(list) => list,
                        Err(e) => {
                            diagnostics::error("[TS:API]", e.clone());
                            s.toast(e);
                            return;
                        }
                    };

                    let total = items.len();
                    s.toast(format!("Импорт раздач: {total}..."));
                    let api = TorrServerApi::from_config(&s.ts_config());
                    let mut ok_count = 0usize;
                    let mut err_count = 0usize;

                    for item in &items {
                        match api.add_backup_item(item) {
                            Ok(_) => ok_count += 1,
                            Err(e) => {
                                err_count += 1;
                                diagnostics::warn(
                                    "[TS:API]",
                                    format!("Не удалось импортировать '{}': {e}", item.title),
                                );
                            }
                        }
                    }

                    diagnostics::info(
                        "[TS:API]",
                        format!(
                            "Импорт из {} завершён: добавлено {ok_count}, ошибок {err_count}",
                            path.display()
                        ),
                    );
                    refresh_torrents_and_status(&s);

                    if err_count == 0 {
                        s.toast(format!("Импортировано раздач: {ok_count}"));
                    } else {
                        s.toast(format!(
                            "Импортировано: {ok_count}, ошибок: {err_count}"
                        ));
                    }
                });
            });
        }

        // 13. Search torrents
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_search_torrents(move |query| {
                let q = query.trim().to_string();
                if q.is_empty() {
                    return;
                }
                if let Some(app) = weak.upgrade() {
                    app.set_search_loading(true);
                    app.set_search_error("".into());
                }
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    run_search(&s, &q);
                });
            });
        }

        // 14. Search filters, source & sort
        {
            let shared = Arc::clone(&self.shared);
            ui.on_set_search_quality(move |q| {
                if let Ok(mut guard) = shared.search_quality.lock() {
                    *guard = q.to_string();
                }
                push_search_to_ui(&shared, "");
            });
        }
        {
            let shared = Arc::clone(&self.shared);
            ui.on_set_search_sort(move |mode| {
                if let Ok(mut guard) = shared.search_sort.lock() {
                    *guard = mode;
                }
                push_search_to_ui(&shared, "");
            });
        }
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_set_search_source(move |mode| {
                if let Ok(mut guard) = shared.search_source.lock() {
                    *guard = mode;
                }
                if let Some(app) = weak.upgrade() {
                    app.set_search_source_mode(mode);
                    let q = app.get_search_query().trim().to_string();
                    if !q.is_empty() {
                        app.set_search_loading(true);
                        app.set_search_error("".into());
                        let s = Arc::clone(&shared);
                        thread::spawn(move || {
                            run_search(&s, &q);
                        });
                    }
                }
            });
        }

        // 15. Add / Play search result
        {
            let shared = Arc::clone(&self.shared);
            ui.on_add_search_result(move |magnet, title| {
                let m = magnet.to_string();
                let t = title.to_string();
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    add_and_optionally_play(&s, &m, Some(&t), false);
                });
            });
        }
        {
            let shared = Arc::clone(&self.shared);
            ui.on_play_search_result(move |magnet, title| {
                let m = magnet.to_string();
                let t = title.to_string();
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    add_and_optionally_play(&s, &m, Some(&t), true);
                });
            });
        }

        // 16. History callbacks
        {
            let shared = Arc::clone(&self.shared);
            ui.on_toggle_history_favorites(move |only_fav| {
                if let Ok(guard) = shared.ui_weak.lock() {
                    if let Some(weak) = guard.as_ref() {
                        if let Some(app) = weak.upgrade() {
                            app.set_history_only_favorites(only_fav);
                            sync_history_to_ui(&shared, &app);
                        }
                    }
                }
            });
        }
        {
            let shared = Arc::clone(&self.shared);
            ui.on_play_history_item(move |hash, magnet| {
                let h = hash.to_string();
                let m = magnet.to_string();
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    let api = TorrServerApi::from_config(&s.ts_config());
                    if api.get_torrent(&h).is_ok() {
                        play_torrent_file(&s, &h, -1);
                    } else if !m.is_empty() {
                        add_and_optionally_play(&s, &m, None, true);
                    } else {
                        let fallback_magnet = format!("magnet:?xt=urn:btih:{h}");
                        add_and_optionally_play(&s, &fallback_magnet, None, true);
                    }
                });
            });
        }
        {
            let shared = Arc::clone(&self.shared);
            ui.on_toggle_history_favorite(move |hash| {
                let h = hash.to_string();
                if let Ok(mut hist) = shared.history.lock() {
                    if let Some(rec) = hist.iter_mut().find(|r| r.hash == h) {
                        rec.favorite = !rec.favorite;
                    }
                    save_history(&shared.paths.ts_history_path, &hist);
                }
                if let Ok(guard) = shared.ui_weak.lock() {
                    if let Some(weak) = guard.as_ref() {
                        if let Some(app) = weak.upgrade() {
                            sync_history_to_ui(&shared, &app);
                        }
                    }
                }
            });
        }
        {
            let shared = Arc::clone(&self.shared);
            ui.on_remove_history_item(move |hash| {
                let h = hash.to_string();
                if let Ok(mut hist) = shared.history.lock() {
                    hist.retain(|r| r.hash != h);
                    save_history(&shared.paths.ts_history_path, &hist);
                }
                if let Ok(guard) = shared.ui_weak.lock() {
                    if let Some(weak) = guard.as_ref() {
                        if let Some(app) = weak.upgrade() {
                            sync_history_to_ui(&shared, &app);
                        }
                    }
                }
            });
        }

        // 17. Connection settings
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_save_ts_connection_settings(move || {
                if let Some(app) = weak.upgrade() {
                    let mode = app.get_ts_mode();
                    let port: u16 = app.get_ts_port().trim().parse().unwrap_or(8090);
                    let remote_url = app.get_ts_remote_url().trim().to_string();
                    let username = app.get_ts_username().to_string();
                    let password = app.get_ts_password().to_string();

                    app.set_is_remote_mode(mode == 1);
                    shared.update_ts_config(|c| {
                        c.mode = mode;
                        c.port = port;
                        c.remote_url = remote_url;
                        c.username = username;
                        c.password = password;
                    });
                    let s = Arc::clone(&shared);
                    thread::spawn(move || {
                        let cfg = s.ts_config();
                        if cfg.mode == 0 {
                            let _ = s.server.restart(&cfg);
                        } else {
                            s.server.stop(&cfg);
                        }
                        thread::sleep(Duration::from_millis(400));
                        refresh_torrents_and_status(&s);
                        s.toast("Настройки подключения применены");
                    });
                }
            });
        }

        // 18. Restart engine
        {
            let shared = Arc::clone(&self.shared);
            ui.on_restart_ts_engine(move || {
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    let cfg = s.ts_config();
                    match s.server.restart(&cfg) {
                        Ok(_) => s.toast("Движок TorrServer перезапущен"),
                        Err(e) => s.toast(e),
                    }
                    thread::sleep(Duration::from_millis(500));
                    refresh_torrents_and_status(&s);
                });
            });
        }

        // 19. Download / Update engine from GitHub
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_download_update_engine(move || {
                if let Some(app) = weak.upgrade() {
                    app.set_engine_updating(true);
                    app.set_engine_update_status("Подготовка к загрузке...".into());
                }
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    let cfg = s.ts_config();
                    let s_cb = Arc::clone(&s);
                    let res = s.server.update_from_github(&cfg, move |status_msg| {
                        let msg = status_msg.to_string();
                        if let Ok(guard) = s_cb.ui_weak.lock() {
                            if let Some(w) = guard.as_ref() {
                                let w_c = w.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(app) = w_c.upgrade() {
                                        app.set_engine_update_status(msg.into());
                                    }
                                });
                            }
                        }
                    });

                    let final_exists = s.server.binary_exists();
                    if let Ok(guard) = s.ui_weak.lock() {
                        if let Some(w) = guard.as_ref() {
                            let w_c = w.clone();
                            let msg = match &res {
                                Ok(tag) => format!("Установлен TorrServer {tag}"),
                                Err(e) => format!("Ошибка обновления: {e}"),
                            };
                            let ok = res.is_ok();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(app) = w_c.upgrade() {
                                    app.set_engine_updating(false);
                                    app.set_engine_missing(!final_exists);
                                    app.set_engine_update_status(msg.clone().into());
                                    core::show_toast(&app, msg);
                                }
                            });
                            if ok {
                                thread::sleep(Duration::from_millis(600));
                                refresh_torrents_and_status(&s);
                            }
                        }
                    }
                });
            });
        }

        // 20. Open web panel
        {
            let shared = Arc::clone(&self.shared);
            ui.on_open_web_panel(move || {
                let url = shared.ts_config().effective_base_url();
                core::open_url_in_browser(&url);
            });
        }

        // 21. Cache & Network settings (/settings)
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_save_ts_cache_settings(move || {
                if let Some(app) = weak.upgrade() {
                    let cache_mb: i64 = app.get_ts_cache_size_mb().trim().parse().unwrap_or(64);
                    let preload: i64 = app.get_ts_preload_percent().trim().parse().unwrap_or(50);
                    let read_ahead: i64 =
                        app.get_ts_read_ahead_percent().trim().parse().unwrap_or(95);
                    let conn_limit: i64 =
                        app.get_ts_connections_limit().trim().parse().unwrap_or(25);
                    let use_disk = app.get_ts_use_disk();
                    let enable_ipv6 = app.get_ts_enable_ipv6();
                    let enable_dlna = app.get_ts_enable_dlna();
                    let enable_rutor = app.get_ts_enable_rutor();

                    shared.update_ts_config(|c| {
                        c.cache_size_mb = cache_mb.max(16);
                        c.preload_percent = preload.clamp(0, 100);
                        c.read_ahead_percent = read_ahead.clamp(0, 100);
                        c.connections_limit = conn_limit.max(5);
                        c.use_disk = use_disk;
                        c.enable_ipv6 = enable_ipv6;
                        c.enable_dlna = enable_dlna;
                        c.enable_rutor = enable_rutor;
                    });

                    let s = Arc::clone(&shared);
                    thread::spawn(move || {
                        let cfg = s.ts_config();
                        let api = TorrServerApi::from_config(&cfg);
                        let sets = TorrServerEngineSettings {
                            cache_size: cfg.cache_size_mb * 1024 * 1024,
                            preload_cache: cfg.preload_percent,
                            reader_read_ahead: cfg.read_ahead_percent,
                            connections_limit: cfg.connections_limit,
                            enable_ipv6: cfg.enable_ipv6,
                            use_disk: cfg.use_disk,
                            enable_dlna: cfg.enable_dlna,
                            enable_rutor_search: cfg.enable_rutor,
                        };
                        match api.set_settings(&sets) {
                            Ok(_) => {
                                diagnostics::info("[TS:API]", "Параметры кеша и сети сохранены на TorrServer");
                                s.toast("Параметры TorrServer применены");
                            }
                            Err(e) => {
                                diagnostics::warn("[TS:API]", e.clone());
                                s.toast(format!("Сохранено локально ({e})"));
                            }
                        }
                    });
                }
            });
        }

        // 22. Detect players
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_detect_players(move || {
                let players = detect_installed_players();
                if let Ok(mut guard) = shared.detected_players.lock() {
                    *guard = players;
                }
                if let Some(app) = weak.upgrade() {
                    sync_players_to_ui(&shared, &app);
                    core::show_toast(&app, "Поиск видеоплееров завершён");
                }
            });
        }

        // 23. Pick custom player exe
        {
            let weak = ui.as_weak();
            ui.on_pick_player_exe(move || {
                let w = weak.clone();
                thread::spawn(move || {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Исполняемые файлы", &["exe"])
                        .pick_file()
                    {
                        let p_str = path.to_string_lossy().to_string();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                app.set_player_custom_path(p_str.into());
                            }
                        });
                    }
                });
            });
        }

        // 24. Save player & clipboard settings
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_save_player_settings(move || {
                if let Some(app) = weak.upgrade() {
                    let idx = app.get_player_selected_idx().max(0) as usize;
                    let player_id = shared
                        .detected_players
                        .lock()
                        .ok()
                        .and_then(|list| list.get(idx).map(|p| p.id.clone()))
                        .unwrap_or_else(|| "auto".to_string());
                    let custom_path = app.get_player_custom_path().trim().to_string();
                    let args = app.get_player_args().to_string();
                    let clip_watch = app.get_clipboard_watch();

                    shared.update_ts_config(|c| {
                        c.player_id = player_id;
                        c.player_custom_path = custom_path;
                        c.player_args = args;
                        c.clipboard_watch = clip_watch;
                    });
                    if let Ok(guard) = shared.clipboard.lock() {
                        if let Some(watcher) = guard.as_ref() {
                            watcher.set_enabled(clip_watch);
                        }
                    }
                    core::show_toast(&app, "Настройки плеера и буфера сохранены");
                }
            });
        }

        // 25. Save & test RuTracker auth
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_save_and_test_rutracker(move || {
                if let Some(app) = weak.upgrade() {
                    let enabled = app.get_rt_enabled();
                    let mirror = app.get_rt_mirror().trim().to_string();
                    let username = app.get_rt_username().trim().to_string();
                    let password = app.get_rt_password().to_string();
                    let proxy = app.get_rt_proxy().trim().to_string();
                    let cookie = app.get_rt_cookie().trim().to_string();

                    shared.update_ts_config(|c| {
                        c.rutracker_enabled = enabled;
                        c.rutracker_mirror = if mirror.is_empty() {
                            "https://rutracker.org".to_string()
                        } else {
                            mirror
                        };
                        c.rutracker_username = username;
                        c.rutracker_password = password;
                        c.rutracker_proxy = proxy;
                        c.rutracker_cookie = cookie;
                    });

                    app.set_rt_testing(true);
                    app.set_rt_auth_status("Проверка авторизации...".into());

                    let s = Arc::clone(&shared);
                    thread::spawn(move || {
                        let cfg = s.ts_config();
                        let res = s.rutracker.verify_login(&cfg);
                        if let Ok(guard) = s.ui_weak.lock() {
                            if let Some(w) = guard.as_ref() {
                                let w_c = w.clone();
                                let (status_msg, ok) = match res {
                                    Ok(msg) => (msg, true),
                                    Err(e) => {
                                        diagnostics::warn("[TS:RUTRACKER]", e.clone());
                                        (e, false)
                                    }
                                };
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(app) = w_c.upgrade() {
                                        app.set_rt_testing(false);
                                        app.set_rt_auth_status(status_msg.clone().into());
                                        core::show_toast(&app, if ok {
                                            "RuTracker: вход выполнен"
                                        } else {
                                            status_msg.as_str()
                                        });
                                    }
                                });
                            }
                        }
                    });
                }
            });
        }
    }

    fn on_window_visibility_changed(&self, visible: bool) {
        self.shared.window_visible.store(visible, Ordering::Relaxed);
        if let Ok(guard) = self.shared.clipboard.lock() {
            if let Some(watcher) = guard.as_ref() {
                watcher.set_paused(!visible);
            }
        }
        if visible {
            let s = Arc::clone(&self.shared);
            thread::spawn(move || {
                refresh_torrents_and_status(&s);
            });
        }
    }

    fn on_shutdown(&mut self) {
        self.shared.poll_stop.store(true, Ordering::Relaxed);
        if let Ok(mut guard) = self.shared.clipboard.lock() {
            if let Some(watcher) = guard.take() {
                watcher.stop();
            }
        }
        let cfg = self.shared.ts_config();
        self.shared.server.stop(&cfg);
    }

    fn status_line(&self) -> ModuleStatus {
        self.shared
            .last_status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    fn diagnostics_snapshot(&self) -> String {
        let cfg = self.shared.ts_config();
        let mode_str = if cfg.mode == 0 { "Local" } else { "Remote" };
        let players_count = self
            .shared
            .detected_players
            .lock()
            .map(|p| p.len())
            .unwrap_or(0);
        format!(
            "  Mode: {} | Endpoint: {} | BinaryExists: {} | DetectedPlayers: {}",
            mode_str,
            cfg.effective_base_url(),
            self.shared.server.binary_exists(),
            players_count
        )
    }

    fn handle_dropped_files(&self, paths: &[PathBuf]) -> bool {
        let torrents: Vec<PathBuf> = paths
            .iter()
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("torrent"))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();

        if torrents.is_empty() {
            return false;
        }

        let s = Arc::clone(&self.shared);
        thread::spawn(move || {
            upload_torrent_files(&s, &torrents);
        });
        true
    }

    fn handle_action(&self, action: &str) -> bool {
        match action {
            "restart_server" => {
                let s = Arc::clone(&self.shared);
                thread::spawn(move || {
                    let cfg = s.ts_config();
                    let _ = s.server.restart(&cfg);
                    thread::sleep(Duration::from_millis(500));
                    refresh_torrents_and_status(&s);
                });
                true
            }
            "open_web_panel" => {
                let url = self.shared.ts_config().effective_base_url();
                core::open_url_in_browser(&url);
                true
            }
            _ => false,
        }
    }
}

fn sync_engine_settings_from_server(shared: &Arc<TorrServerShared>) {
    let cfg = shared.ts_config();
    let api = TorrServerApi::from_config(&cfg);
    if let Ok(sets) = api.get_settings() {
        let cache_mb = (sets.cache_size / (1024 * 1024)).max(16);
        shared.update_ts_config(|c| {
            c.cache_size_mb = cache_mb;
            c.preload_percent = sets.preload_cache;
            c.read_ahead_percent = sets.reader_read_ahead;
            c.connections_limit = sets.connections_limit;
            c.use_disk = sets.use_disk;
            c.enable_ipv6 = sets.enable_ipv6;
            c.enable_dlna = sets.enable_dlna;
            c.enable_rutor = sets.enable_rutor_search;
        });

        if let Ok(guard) = shared.ui_weak.lock() {
            if let Some(weak) = guard.as_ref() {
                let w = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = w.upgrade() {
                        app.set_ts_cache_size_mb(cache_mb.to_string().into());
                        app.set_ts_preload_percent(sets.preload_cache.to_string().into());
                        app.set_ts_read_ahead_percent(sets.reader_read_ahead.to_string().into());
                        app.set_ts_connections_limit(sets.connections_limit.to_string().into());
                        app.set_ts_use_disk(sets.use_disk);
                        app.set_ts_enable_ipv6(sets.enable_ipv6);
                        app.set_ts_enable_dlna(sets.enable_dlna);
                        app.set_ts_enable_rutor(sets.enable_rutor_search);
                    }
                });
            }
        }
    }
}

fn add_and_optionally_play(
    shared: &Arc<TorrServerShared>,
    link: &str,
    title: Option<&str>,
    auto_play: bool,
) {
    let cfg = shared.ts_config();
    let api = TorrServerApi::from_config(&cfg);

    let result = if let Some(topic_id) = link.strip_prefix("rutracker://topic/") {
        shared.toast("Загрузка торрента с RuTracker...");
        match shared.rutracker.fetch_torrent_or_magnet(&cfg, topic_id) {
            Ok(RuTrackerPayload::TorrentBytes { bytes, filename }) => {
                api.upload_torrent_bytes(bytes, filename)
            }
            Ok(RuTrackerPayload::Magnet(magnet)) => api.add_magnet(&magnet, title),
            Err(e) => Err(e),
        }
    } else {
        api.add_magnet(link, title)
    };

    match result {
        Ok(t) => {
            let hash = t.hash.clone();
            let disp = t.display_title();
            diagnostics::info("[TS:API]", format!("Добавлена раздача: {disp} ({hash})"));
            if let Ok(mut sel) = shared.selected_hash.lock() {
                *sel = hash.clone();
            }
            shared.toast(format!("Добавлено: {disp}"));
            refresh_torrents_and_status(shared);
            if auto_play {
                // Brief wait so TorrServer retrieves file list metadata if needed
                thread::sleep(Duration::from_millis(600));
                play_torrent_file(shared, &hash, -1);
            }
        }
        Err(e) => {
            diagnostics::error("[TS:API]", e.clone());
            shared.toast(e);
        }
    }
}

fn upload_torrent_files(shared: &Arc<TorrServerShared>, paths: &[PathBuf]) {
    let api = TorrServerApi::from_config(&shared.ts_config());
    for p in paths {
        match api.upload_torrent_file(p) {
            Ok(t) => {
                let disp = t.display_title();
                diagnostics::info("[TS:API]", format!("Загружен .torrent: {disp}"));
                if let Ok(mut sel) = shared.selected_hash.lock() {
                    *sel = t.hash.clone();
                }
                shared.toast(format!("Загружен: {disp}"));
            }
            Err(e) => {
                diagnostics::error("[TS:API]", e.clone());
                shared.toast(e);
            }
        }
    }
    refresh_torrents_and_status(shared);
}

fn play_torrent_file(shared: &Arc<TorrServerShared>, hash: &str, requested_file_id: i32) {
    let cfg = shared.ts_config();
    let api = TorrServerApi::from_config(&cfg);
    let torrent = match api.get_torrent(hash) {
        Ok(t) => t,
        Err(e) => {
            diagnostics::error("[TS:PLAYER]", e.clone());
            shared.toast(e);
            return;
        }
    };

    let files = torrent.files();
    if files.is_empty() {
        shared.toast("Список файлов раздачи пока пуст");
        return;
    }

    let chosen = if requested_file_id >= 0 {
        files.iter().find(|f| f.id == requested_file_id).or_else(|| files.first())
    } else {
        files
            .iter()
            .find(|f| is_video_filename(&f.path))
            .or_else(|| files.first())
    };

    let Some(file) = chosen else {
        shared.toast("Файл не найден");
        return;
    };

    let base_name = std::path::Path::new(&file.path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("stream.mkv");
    let stream_url = api.stream_url(hash, file.id, base_name);

    let player_res = {
        let detected = shared.detected_players.lock().map(|d| d.clone()).unwrap_or_default();
        let idx = detected
            .iter()
            .position(|p| p.id == cfg.player_id)
            .unwrap_or(0);
        resolve_player_exe(&cfg.player_custom_path, idx, &detected)
    };

    match player_res {
        Ok(exe) => {
            if let Err(e) = launch_player(&exe, &stream_url, &cfg.player_args) {
                diagnostics::error("[TS:PLAYER]", e.clone());
                shared.toast(e);
                return;
            }
            let magnet = format!("magnet:?xt=urn:btih:{hash}");
            let video_count = files.iter().filter(|f| is_video_filename(&f.path)).count();
            if let Ok(mut hist) = shared.history.lock() {
                upsert_history_watch(
                    &mut hist,
                    hash,
                    &torrent.display_title(),
                    &magnet,
                    video_count.max(files.len()),
                    Some(file.id),
                );
                save_history(&shared.paths.ts_history_path, &hist);
            }
            push_torrents_to_ui(shared);
            if let Ok(guard) = shared.ui_weak.lock() {
                if let Some(w) = guard.as_ref() {
                    let w_c = w.clone();
                    let s_c = Arc::clone(shared);
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(app) = w_c.upgrade() {
                            sync_history_to_ui(&s_c, &app);
                        }
                    });
                }
            }
        }
        Err(e) => {
            diagnostics::error("[TS:PLAYER]", e.clone());
            shared.toast(e);
        }
    }
}

fn play_torrent_m3u(shared: &Arc<TorrServerShared>, hash: &str) {
    let cfg = shared.ts_config();
    let api = TorrServerApi::from_config(&cfg);
    let torrent = match api.get_torrent(hash) {
        Ok(t) => t,
        Err(e) => {
            diagnostics::error("[TS:PLAYER]", e.clone());
            shared.toast(e);
            return;
        }
    };

    let mut files = torrent.files();
    let videos: Vec<_> = files
        .iter()
        .filter(|f| is_video_filename(&f.path))
        .cloned()
        .collect();
    if !videos.is_empty() {
        files = videos;
    }
    if files.is_empty() {
        shared.toast("Нет файлов для плейлиста .m3u");
        return;
    }

    let entries: Vec<(String, String)> = files
        .iter()
        .map(|f| {
            let name = std::path::Path::new(&f.path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&f.path)
                .to_string();
            let url = api.stream_url(hash, f.id, &name);
            (name, url)
        })
        .collect();

    let m3u_path = match write_m3u_playlist(&shared.paths.ts_playlists_dir, hash, &entries) {
        Ok(p) => p,
        Err(e) => {
            diagnostics::error("[TS:PLAYER]", e.clone());
            shared.toast(e);
            return;
        }
    };

    let player_res = {
        let detected = shared.detected_players.lock().map(|d| d.clone()).unwrap_or_default();
        let idx = detected
            .iter()
            .position(|p| p.id == cfg.player_id)
            .unwrap_or(0);
        resolve_player_exe(&cfg.player_custom_path, idx, &detected)
    };

    match player_res {
        Ok(exe) => {
            let target = m3u_path.to_string_lossy().to_string();
            if let Err(e) = launch_player(&exe, &target, &cfg.player_args) {
                diagnostics::error("[TS:PLAYER]", e.clone());
                shared.toast(e);
                return;
            }
            shared.toast(format!("Плейлист из {} серий открыт в плеере", entries.len()));
        }
        Err(e) => {
            diagnostics::error("[TS:PLAYER]", e.clone());
            shared.toast(e);
        }
    }
}

fn refresh_torrents_and_status(shared: &Arc<TorrServerShared>) {
    let cfg = shared.ts_config();
    let api = TorrServerApi::from_config(&cfg);
    let echo = api.echo();

    let mut status = ModuleStatus::default();
    match echo {
        Ok((ver, ping_ms)) => {
            status.healthy = true;
            status.state_label = format!("Подключён ({ping_ms} мс)");
            status.version_label = ver;

            if let Ok(list) = api.list_torrents() {
                let total_speed: f64 = list.iter().map(|t| t.download_speed).sum();
                status.speed_label = format_speed(total_speed);
                status.active_count = list.len() as i32;
                if let Ok(mut cache) = shared.cached_torrents.lock() {
                    *cache = list;
                }
            }
        }
        Err(_) => {
            status.healthy = false;
            status.state_label = if cfg.mode == 0 && !shared.server.binary_exists() {
                "Движок не установлен".to_string()
            } else {
                "Нет связи с TorrServer".to_string()
            };
        }
    }

    if let Ok(mut st_guard) = shared.last_status.lock() {
        *st_guard = status.clone();
    }

    push_torrents_to_ui(shared);
}

fn push_torrents_to_ui(shared: &Arc<TorrServerShared>) {
    let cfg = shared.ts_config();
    let api = TorrServerApi::from_config(&cfg);
    let torrents = shared
        .cached_torrents
        .lock()
        .map(|c| c.clone())
        .unwrap_or_default();
    let selected = shared
        .selected_hash
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();
    let status = shared
        .last_status
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();
    let watched_ids: Vec<i32> = shared
        .history
        .lock()
        .ok()
        .and_then(|h| {
            h.iter()
                .find(|r| r.hash == selected)
                .map(|r| r.watched_file_ids.clone())
        })
        .unwrap_or_default();
    let binary_missing = !shared.server.binary_exists();

    let ui_torrents: Vec<TorrentItem> = torrents
        .iter()
        .map(|t| {
            let progress = if t.torrent_size > 0 {
                (t.loaded_size as f64 / t.torrent_size as f64) as f32
            } else {
                0.0
            };
            TorrentItem {
                hash: t.hash.clone().into(),
                title: t.display_title().into(),
                size_text: format_bytes(t.torrent_size).into(),
                speed_text: format_speed(t.download_speed).into(),
                peers_text: format!("{} / {}", t.active_peers, t.connected_seeders).into(),
                progress,
                status_text: if t.stat_string.is_empty() {
                    "Активен".into()
                } else {
                    t.stat_string.clone().into()
                },
                magnet: format!("magnet:?xt=urn:btih:{}", t.hash).into(),
                is_selected: t.hash == selected,
            }
        })
        .collect();

    let mut selected_title = String::new();
    let mut ui_files: Vec<TorrentFileItem> = Vec::new();
    if let Some(sel_t) = torrents.iter().find(|t| t.hash == selected) {
        selected_title = sel_t.display_title();
        for (idx, f) in sel_t.files().into_iter().enumerate() {
            let is_vid = is_video_filename(&f.path);
            if cfg.only_video_files && !is_vid {
                continue;
            }
            let file_name = std::path::Path::new(&f.path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&f.path)
                .to_string();
            let stream = api.stream_url(&sel_t.hash, f.id, &file_name);
            ui_files.push(TorrentFileItem {
                id: f.id,
                index: idx as i32,
                name: file_name.into(),
                path: f.path.into(),
                size_text: format_bytes(f.length).into(),
                stream_url: stream.into(),
                is_video: is_vid,
                is_watched: watched_ids.contains(&f.id),
            });
        }
    }

    if let Ok(guard) = shared.ui_weak.lock() {
        if let Some(weak) = guard.as_ref() {
            let w_c = weak.clone();
            let sel_hash = selected;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = w_c.upgrade() {
                    app.set_torrents(ModelRc::from(Rc::new(VecModel::from(ui_torrents))));
                    app.set_selected_torrent_hash(sel_hash.into());
                    app.set_selected_torrent_title(selected_title.into());
                    app.set_selected_files(ModelRc::from(Rc::new(VecModel::from(ui_files))));
                    app.set_engine_missing(binary_missing);

                    app.set_status_healthy(status.healthy);
                    app.set_status_text(status.state_label.into());
                    app.set_status_version(status.version_label.clone().into());
                    app.set_status_speed(status.speed_label.into());
                    app.set_status_active_count(status.active_count);
                    if !status.version_label.is_empty() {
                        app.set_ts_engine_version_text(status.version_label.into());
                    }
                }
            });
        }
    }
}

fn run_search(shared: &Arc<TorrServerShared>, query: &str) {
    let cfg = shared.ts_config();
    let api = TorrServerApi::from_config(&cfg);
    let source_mode = shared.search_source.lock().map(|m| *m).unwrap_or(0);

    let mut combined: Vec<ApiSearchResult> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    match source_mode {
        // 1: Only RuTor
        1 => match api.search_rutor(query) {
            Ok(list) => combined = list,
            Err(e) => notes.push(e),
        },
        // 2: Only RuTracker
        2 => match shared.rutracker.search(&cfg, query) {
            Ok(list) => combined = list,
            Err(e) => notes.push(e),
        },
        // 3: Only JacRed
        3 => match api.search_jacred(query) {
            Ok(list) => combined = list,
            Err(e) => notes.push(format!("JacRed: {e}")),
        },
        // 0: All enabled sources + JacRed fallback
        _ => {
            match api.search_rutor(query) {
                Ok(mut list) => combined.append(&mut list),
                Err(e) => notes.push(e),
            }

            if cfg.rutracker_enabled && cfg.has_rutracker_credentials() {
                match shared.rutracker.search(&cfg, query) {
                    Ok(mut rt_list) => combined.append(&mut rt_list),
                    Err(e) => {
                        diagnostics::warn("[TS:RUTRACKER]", e.clone());
                        notes.push(format!("RuTracker: {e}"));
                    }
                }
            }

            if combined.is_empty() {
                let reason = if notes.is_empty() {
                    "источники вернули 0 результатов".to_string()
                } else {
                    notes.join("; ")
                };
                diagnostics::info(
                    "[TS:API]",
                    format!("{reason} — выполняется fallback-поиск JacRed"),
                );
                match api.search_jacred(query) {
                    Ok(mut jr_list) => combined.append(&mut jr_list),
                    Err(e) => notes.push(format!("JacRed: {e}")),
                }
            }
        }
    }

    if !combined.is_empty() {
        if let Ok(mut raw) = shared.search_raw.lock() {
            *raw = combined;
        }
        push_search_to_ui(shared, "");
    } else {
        if let Ok(mut raw) = shared.search_raw.lock() {
            raw.clear();
        }
        let err_text = if notes.is_empty() {
            "По вашему запросу ничего не найдено".to_string()
        } else {
            let msg = format!("Поиск не дал результатов ({})", notes.join("; "));
            diagnostics::error("[TS:API]", msg.clone());
            msg
        };
        push_search_to_ui(shared, &err_text);
    }
}

fn push_search_to_ui(shared: &Arc<TorrServerShared>, err_msg: &str) {
    let quality = shared
        .search_quality
        .lock()
        .map(|q| q.clone())
        .unwrap_or_else(|_| "ALL".to_string());
    let sort_mode = shared.search_sort.lock().map(|s| *s).unwrap_or(0);
    let source_mode = shared.search_source.lock().map(|s| *s).unwrap_or(0);
    let raw = shared
        .search_raw
        .lock()
        .map(|r| r.clone())
        .unwrap_or_default();

    let mut filtered: Vec<ApiSearchResult> = raw
        .into_iter()
        .filter(|r| {
            if quality == "ALL" {
                true
            } else {
                r.title.to_lowercase().contains(&quality.to_lowercase())
                    || r.quality.to_lowercase().contains(&quality.to_lowercase())
            }
        })
        .collect();

    if sort_mode == 1 {
        filtered.sort_by(|a, b| {
            b.size_bytes
                .partial_cmp(&a.size_bytes)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    } else {
        filtered.sort_by(|a, b| b.seeds.cmp(&a.seeds));
    }

    let ui_items: Vec<SearchResultItem> = filtered
        .into_iter()
        .map(|r| SearchResultItem {
            title: r.title.into(),
            magnet: r.magnet.into(),
            size_text: r.size_text.into(),
            size_bytes: r.size_bytes as f32,
            seeds: r.seeds,
            peers: r.peers,
            quality: r.quality.into(),
            tracker: r.tracker.into(),
        })
        .collect();

    let err_owned = err_msg.to_string();
    if let Ok(guard) = shared.ui_weak.lock() {
        if let Some(weak) = guard.as_ref() {
            let w_c = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = w_c.upgrade() {
                    app.set_search_loading(false);
                    app.set_search_error(err_owned.into());
                    app.set_search_quality_filter(quality.into());
                    app.set_search_sort_mode(sort_mode);
                    app.set_search_source_mode(source_mode);
                    app.set_search_results(ModelRc::from(Rc::new(VecModel::from(ui_items))));
                }
            });
        }
    }
}

fn sync_players_to_ui(shared: &Arc<TorrServerShared>, ui: &AppWindow) {
    let cfg = shared.ts_config();
    let detected = shared
        .detected_players
        .lock()
        .map(|p| p.clone())
        .unwrap_or_default();

    let mut presets: Vec<SharedString> = detected
        .iter()
        .map(|p| format!("{} ({})", p.name, p.exe_path.display()).into())
        .collect();

    if presets.is_empty() {
        presets.push("Автопоиск: плееры не найдены".into());
        ui.set_player_detect_status(
            "PotPlayer / MPV / VLC / MPC не найдены автоматически — выберите .exe вручную."
                .into(),
        );
    } else {
        ui.set_player_detect_status(
            format!("Обнаружено плееров: {}", detected.len()).into(),
        );
    }

    let sel_idx = detected
        .iter()
        .position(|p| p.id == cfg.player_id)
        .unwrap_or(0) as i32;

    ui.set_player_presets(ModelRc::from(Rc::new(VecModel::from(presets))));
    ui.set_player_selected_idx(sel_idx);
}

fn sync_history_to_ui(shared: &Arc<TorrServerShared>, ui: &AppWindow) {
    let only_fav = ui.get_history_only_favorites();
    let items: Vec<HistoryItem> = shared
        .history
        .lock()
        .map(|list| {
            list.iter()
                .filter(|r| !only_fav || r.favorite)
                .map(|r| HistoryItem {
                    hash: r.hash.clone().into(),
                    title: r.title.clone().into(),
                    magnet: r.magnet.clone().into(),
                    last_opened: r.last_opened.clone().into(),
                    watched_count: r.watched_file_ids.len() as i32,
                    total_files: r.total_files as i32,
                    favorite: r.favorite,
                })
                .collect()
        })
        .unwrap_or_default();

    ui.set_history_items(ModelRc::from(Rc::new(VecModel::from(items))));
}
