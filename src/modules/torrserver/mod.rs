pub mod api;
pub mod clipboard;
pub mod metadata;
pub mod player;
pub mod rutracker;
pub mod rutracker_webview;
pub mod server_manager;
pub mod ui_adapter;

use crate::core::diagnostics;
use crate::core::storage::{
    save_config, AppConfig, PortablePaths, SavedTorrentTask, TorrServerConfig, TorrentOriginMeta,
};
use crate::modules::torrserver::ui_adapter::{
    build_ui_file_item, build_ui_torrent_item, matches_filter, update_telemetry_and_counters,
    watch_trailer,
};
use crate::platform::windows::open_url_in_browser;
use crate::ui_bridge::show_toast;
use crate::modules::torrserver::api::{
    extract_torrent_comment_url, format_bytes, format_speed, format_unix_datetime_short,
    parse_backup_json, ApiSearchResult, ApiTorrent, TorrentBackupItem, TorrServerApi,
    TorrServerEngineSettings,
};
use crate::modules::torrserver::clipboard::ClipboardWatcher;
use crate::modules::torrserver::metadata::{
    extract_rutracker_topic_id_from_filename, fetch_rich_metadata, DecodedRgbaImage,
    TorrentRichMetadata,
};
use crate::modules::torrserver::player::{
    detect_installed_players, is_video_filename, launch_player, load_history, open_with_os_default,
    resolve_player_exe, save_history, upsert_history_watch, write_m3u_playlist, DetectedPlayer,
    HistoryRecord,
};
use crate::modules::torrserver::rutracker::{RuTrackerClient, RuTrackerPayload};
use crate::modules::torrserver::server_manager::{
    default_matrix_releases, fetch_available_matrix_releases, MatrixReleaseOption, ServerManager,
};
use crate::modules::{AppModule, ModuleInfo, ModuleStatus};
use crate::{AppWindow, HistoryItem, ScreenshotItem, SearchResultItem, TorrentFileItem, TorrentItem};
use chrono::Utc;
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

struct TorrServerShared {
    paths: PortablePaths,
    config: Arc<Mutex<AppConfig>>,
    server: ServerManager,
    ui_weak: Mutex<Option<slint::Weak<AppWindow>>>,
    window_visible: Arc<AtomicBool>,
    poll_stop: Arc<AtomicBool>,
    engine_updating: Arc<AtomicBool>,
    cached_torrents: Mutex<Vec<ApiTorrent>>,
    torrents_meta: Mutex<HashMap<String, i64>>,
    torrents_links: Mutex<HashMap<String, TorrentOriginMeta>>,
    saved_torrents: Mutex<HashMap<String, SavedTorrentTask>>,
    server_seen_hashes: Mutex<HashSet<String>>,
    needs_server_restore: AtomicBool,
    restore_lock: Mutex<()>,
    detail_meta_cache: Mutex<HashMap<String, TorrentRichMetadata>>,
    detail_generation: Arc<AtomicU64>,
    selected_hash: Mutex<String>,
    search_raw: Mutex<Vec<ApiSearchResult>>,
    search_quality: Mutex<String>,
    search_sort: Mutex<i32>,
    search_source: Mutex<i32>,
    rutracker: RuTrackerClient,
    detected_players: Mutex<Vec<DetectedPlayer>>,
    available_releases: Mutex<Vec<MatrixReleaseOption>>,
    history: Mutex<Vec<HistoryRecord>>,
    last_status: Mutex<ModuleStatus>,
    last_list_err_log: Mutex<Option<Instant>>,
    clipboard: Mutex<Option<ClipboardWatcher>>,
    filter_mode: AtomicI32,
}

impl TorrServerShared {
    fn ts_config(&self) -> TorrServerConfig {
        self.config
            .lock()
            .map(|c| c.torrserver.clone())
            .unwrap_or_default()
    }

    fn keep_in_memory(&self) -> bool {
        self.config
            .lock()
            .map(|c| c.core.keep_in_memory)
            .unwrap_or(true)
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
                        show_toast(&app, msg);
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
        let torrents_meta = load_torrents_meta(&paths.ts_torrents_meta_path);
        let torrents_links = load_torrents_links(&paths.ts_torrents_links_path);
        let saved_torrents = load_torrents_saved(&paths.ts_torrents_saved_path);
        let initial_cached = build_placeholder_torrents(&saved_torrents);
        let players = detect_installed_players();
        let server = ServerManager::new(paths.clone());
        let rutracker = RuTrackerClient::new(paths.ts_webview_dir.clone());

        let shared = Arc::new(TorrServerShared {
            paths,
            config,
            server,
            ui_weak: Mutex::new(None),
            window_visible: Arc::new(AtomicBool::new(true)),
            poll_stop: Arc::new(AtomicBool::new(false)),
            engine_updating: Arc::new(AtomicBool::new(false)),
            cached_torrents: Mutex::new(initial_cached),
            torrents_meta: Mutex::new(torrents_meta),
            torrents_links: Mutex::new(torrents_links),
            saved_torrents: Mutex::new(saved_torrents),
            server_seen_hashes: Mutex::new(HashSet::new()),
            needs_server_restore: AtomicBool::new(true),
            restore_lock: Mutex::new(()),
            detail_meta_cache: Mutex::new(HashMap::new()),
            detail_generation: Arc::new(AtomicU64::new(0)),
            selected_hash: Mutex::new(String::new()),
            search_raw: Mutex::new(Vec::new()),
            search_quality: Mutex::new("ALL".to_string()),
            search_sort: Mutex::new(0),
            search_source: Mutex::new(0),
            rutracker,
            detected_players: Mutex::new(players),
            available_releases: Mutex::new(default_matrix_releases()),
            history: Mutex::new(history),
            last_status: Mutex::new(ModuleStatus::default()),
            last_list_err_log: Mutex::new(None),
            clipboard: Mutex::new(None),
            filter_mode: AtomicI32::new(0),
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
        let high_prio = self.shared.keep_in_memory();
        if cfg.mode == 0 {
            if self.shared.server.binary_exists() {
                if let Err(e) = self.shared.server.start(&cfg, high_prio) {
                    diagnostics::error("[TS:ENGINE]", e);
                }
            } else {
                diagnostics::warn(
                    "[TS:ENGINE]",
                    "TorrServer-windows-amd64.exe отсутствует в data/torrserver/ — выберите версию и загрузите движок",
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
                        let hash = if !t.hash.trim().is_empty() {
                            t.hash.clone()
                        } else {
                            extract_btih(&magnet).unwrap_or_default()
                        };
                        let title = t.display_title();
                        let ts = record_torrent_timestamp(&shared_add, &hash, t.timestamp);
                        record_torrent_origin(&shared_add, &hash, Some(&magnet), None, None, None, None);
                        record_saved_torrent(
                            &shared_add,
                            &hash,
                            &title,
                            Some(&magnet),
                            None,
                            &t.poster,
                            &t.data,
                            ts,
                        );
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

        // Background GitHub MatriX release list fetch
        let shared_rel = Arc::clone(&self.shared);
        thread::Builder::new()
            .name("ts-gh-releases".to_string())
            .spawn(move || {
                refresh_github_releases_bg(&shared_rel, false);
            })
            .ok();

        // Start background UI polling + Engine Watchdog thread
        let shared_poll = Arc::clone(&self.shared);
        thread::Builder::new()
            .name("ts-ui-poller".to_string())
            .spawn(move || {
                // Give engine a brief moment to boot on startup if not yet listening
                thread::sleep(Duration::from_millis(450));
                sync_engine_settings_from_server(&shared_poll);
                let mut bg_watchdog_ticks = 0u32;
                let mut consec_echo_fail = 0u32;
                while !shared_poll.poll_stop.load(Ordering::Relaxed) {
                    let visible = shared_poll.window_visible.load(Ordering::Relaxed);
                    let needs_restore = shared_poll.needs_server_restore.load(Ordering::SeqCst);
                    if visible || needs_restore {
                        refresh_torrents_and_status(&shared_poll);
                        maybe_run_watchdog(&shared_poll, &mut consec_echo_fail);
                        thread::sleep(Duration::from_millis(1600));
                    } else {
                        bg_watchdog_ticks = bg_watchdog_ticks.wrapping_add(1);
                        if bg_watchdog_ticks % 4 == 0 {
                            maybe_run_watchdog(&shared_poll, &mut consec_echo_fail);
                        }
                        thread::sleep(Duration::from_millis(850));
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
        ui.set_engine_watchdog(cfg.engine_watchdog);
        ui.set_keep_engine_on_exit(cfg.keep_engine_on_exit);
        ui.set_kp_api_key(cfg.kinopoisk_api_key.clone().into());
        ui.set_torrents_sort_mode(cfg.torrents_sort_mode);
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
        ui.set_rt_use_webview(cfg.rutracker_use_webview);
        ui.set_rt_mirror(cfg.rutracker_mirror.clone().into());
        ui.set_rt_username(cfg.rutracker_username.clone().into());
        ui.set_rt_password(cfg.rutracker_password.clone().into());
        ui.set_rt_proxy(cfg.rutracker_proxy.clone().into());
        ui.set_rt_cookie(cfg.rutracker_cookie.clone().into());
        ui.set_rt_user_agent(cfg.rutracker_user_agent.clone().into());
        if cfg.rutracker_enabled && cfg.has_rutracker_credentials() {
            ui.set_rt_auth_status(
                if cfg.rutracker_use_webview {
                    "Активен режим встроенного браузера (WebView2)"
                } else {
                    "Учётные данные сохранены"
                }
                .into(),
            );
        }

        apply_addresses_to_window(&cfg, ui);
        sync_releases_to_ui(&self.shared, ui);
        sync_players_to_ui(&self.shared, ui);
        sync_history_to_ui(&self.shared, ui);
        push_torrents_to_ui(&self.shared);

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
                    add_and_optionally_play(&s, &link, None, None, false);
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
                                add_and_optionally_play(&s, &trimmed, None, None, false);
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
                let h = hash.to_string();
                let changed = if let Ok(mut sel) = shared.selected_hash.lock() {
                    let diff = *sel != h;
                    *sel = h.clone();
                    diff
                } else {
                    true
                };
                push_torrents_to_ui(&shared);
                if changed || !h.is_empty() {
                    load_torrent_details_async(&shared, &h, false);
                }
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
                    let rem_res = api.remove_torrent(&h);
                    let removed_or_absent = match &rem_res {
                        Ok(_) => true,
                        Err(_) => {
                            // If server is offline or no longer has this hash, still allow removing from local store.
                            api.echo().is_err() || api.get_torrent(&h).is_err()
                        }
                    };

                    if removed_or_absent {
                        remove_saved_torrent(&s, &h);
                        if let Ok(mut cache) = s.cached_torrents.lock() {
                            cache.retain(|t| !t.hash.eq_ignore_ascii_case(&h));
                        }
                        diagnostics::info("[TS:API]", format!("Раздача удалена: {h}"));
                        if let Ok(mut sel) = s.selected_hash.lock() {
                            if sel.eq_ignore_ascii_case(&h) {
                                sel.clear();
                                clear_detail_ui(&s);
                            }
                        }
                        s.toast("Раздача удалена");
                        refresh_torrents_and_status(&s);
                    } else if let Err(e) = rem_res {
                        diagnostics::error("[TS:API]", e.clone());
                        s.toast(e);
                    }
                });
            });
        }

        // 8. Copy magnet (prefers saved original magnet link with trackers)
        {
            let shared = Arc::clone(&self.shared);
            ui.on_copy_torrent_magnet(move |hash| {
                let h = hash.to_string();
                let magnet = resolve_best_magnet(&shared, &h);
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(magnet);
                    shared.toast("Magnet-ссылка скопирована");
                }
            });
        }

        // 8a. Download playlist (.m3u) of a single torrent
        {
            let shared = Arc::clone(&self.shared);
            ui.on_download_torrent_playlist(move |hash| {
                let h = hash.to_string();
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    let api = TorrServerApi::from_config(&s.ts_config());
                    let title = s
                        .cached_torrents
                        .lock()
                        .ok()
                        .and_then(|list| list.iter().find(|t| t.hash == h).map(|t| t.display_title()))
                        .unwrap_or_else(|| h.clone());
                    let safe_name = sanitize_filename(&title);
                    match api.get_playlist_m3u(&h) {
                        Ok(m3u) => {
                            if let Some(path) = rfd::FileDialog::new()
                                .set_file_name(&format!("{safe_name}.m3u"))
                                .add_filter("Плейлист M3U", &["m3u"])
                                .save_file()
                            {
                                match std::fs::write(&path, m3u) {
                                    Ok(_) => s.toast(format!("Плейлист сохранён: {}", path.display())),
                                    Err(e) => s.toast(format!("Ошибка сохранения плейлиста: {e}")),
                                }
                            }
                        }
                        Err(e) => {
                            diagnostics::warn("[TS:API]", e.clone());
                            s.toast(e);
                        }
                    }
                });
            });
        }

        // 8b. Download playlist (.m3u) of ALL torrents on server
        {
            let shared = Arc::clone(&self.shared);
            ui.on_download_all_playlist(move || {
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    let api = TorrServerApi::from_config(&s.ts_config());
                    match api.get_all_playlist_m3u() {
                        Ok(m3u) => {
                            if let Some(path) = rfd::FileDialog::new()
                                .set_file_name("TorrServer_All.m3u")
                                .add_filter("Плейлист M3U", &["m3u"])
                                .save_file()
                            {
                                match std::fs::write(&path, m3u) {
                                    Ok(_) => s.toast(format!("Общий плейлист сохранён: {}", path.display())),
                                    Err(e) => s.toast(format!("Ошибка сохранения плейлиста: {e}")),
                                }
                            }
                        }
                        Err(e) => {
                            diagnostics::warn("[TS:API]", e.clone());
                            s.toast(e);
                        }
                    }
                });
            });
        }

        // 8c. Refresh single torrent (drop + get + re-fetch rich metadata)
        {
            let shared = Arc::clone(&self.shared);
            ui.on_refresh_single_torrent(move |hash| {
                let h = hash.to_string();
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    let api = TorrServerApi::from_config(&s.ts_config());
                    match api.refresh_torrent(&h) {
                        Ok(_) => {
                            s.toast("Торрент обновлён");
                            refresh_torrents_and_status(&s);
                            let is_sel = s
                                .selected_hash
                                .lock()
                                .map(|sel| *sel == h)
                                .unwrap_or(false);
                            if is_sel {
                                load_torrent_details_async(&s, &h, true);
                            }
                        }
                        Err(e) => {
                            diagnostics::warn("[TS:API]", e.clone());
                            s.toast(e);
                        }
                    }
                });
            });
        }

        // 8d. Show torrent info modal
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_show_torrent_info(move |hash| {
                let h = hash.to_string();
                let cfg = shared.ts_config();
                let torrent_opt = shared
                    .cached_torrents
                    .lock()
                    .ok()
                    .and_then(|list| list.iter().find(|t| t.hash == h).cloned());
                let added_ts = shared
                    .torrents_meta
                    .lock()
                    .ok()
                    .and_then(|m| m.get(&h.trim().to_lowercase()).copied())
                    .unwrap_or(0);
                let tracker_url = resolve_tracker_url(&shared, &cfg, &h).unwrap_or_default();
                let magnet = resolve_best_magnet(&shared, &h);
                let ratings = shared
                    .detail_meta_cache
                    .lock()
                    .ok()
                    .and_then(|m| m.get(&h.trim().to_lowercase()).cloned())
                    .map(|meta| {
                        let mut parts = Vec::new();
                        if !meta.rating_kp.is_empty() {
                            parts.push(format!("Кинопоиск: {}", meta.rating_kp));
                        }
                        if !meta.rating_imdb.is_empty() {
                            parts.push(format!("IMDb: {}", meta.rating_imdb));
                        }
                        parts.join("   •   ")
                    })
                    .unwrap_or_default();

                if let Some(app) = weak.upgrade() {
                    if let Some(t) = torrent_opt {
                        let ts = if t.timestamp > 900_000_000 { t.timestamp } else { added_ts };
                        app.set_info_hash(t.hash.clone().into());
                        app.set_info_title(t.display_title().into());
                        app.set_info_size(format_bytes(t.torrent_size).into());
                        app.set_info_added(format_unix_datetime_short(ts).into());
                        app.set_info_status(
                            if t.stat_string.is_empty() {
                                "Активен".to_string()
                            } else {
                                t.stat_string.clone()
                            }
                            .into(),
                        );
                        app.set_info_speed(format_speed(t.download_speed).into());
                        app.set_info_peers(format!("{} / {}", t.active_peers, t.connected_seeders).into());
                        app.set_info_files_count(t.files().len().to_string().into());
                    } else {
                        app.set_info_hash(h.clone().into());
                        app.set_info_title(h.clone().into());
                        app.set_info_size("—".into());
                        app.set_info_added("".into());
                        app.set_info_status("—".into());
                        app.set_info_speed("0 КБ/с".into());
                        app.set_info_peers("0 / 0".into());
                        app.set_info_files_count("0".into());
                    }
                    app.set_info_tracker_url(tracker_url.into());
                    app.set_info_ratings(ratings.into());
                    app.set_info_magnet(magnet.into());
                    app.set_torrent_info_visible(true);
                }
            });
        }

        // 8e. Open tracker page in browser
        {
            let shared = Arc::clone(&self.shared);
            ui.on_open_tracker_page(move |hash| {
                let h = hash.to_string();
                let cfg = shared.ts_config();
                if let Some(url) = resolve_tracker_url(&shared, &cfg, &h) {
                    open_url_in_browser(&url);
                } else {
                    shared.toast("Страница на трекере не найдена (раздача добавлена без ссылки на тему)");
                }
            });
        }

        // 8f. Refresh rich movie metadata manually
        {
            let shared = Arc::clone(&self.shared);
            ui.on_refresh_torrent_metadata(move |hash| {
                let h = hash.to_string();
                if !h.is_empty() {
                    load_torrent_details_async(&shared, &h, true);
                }
            });
        }

        // 8g. Open arbitrary external URL (from screenshot lightbox)
        {
            ui.on_open_external_url(move |url| {
                let u = url.trim();
                if u.starts_with("http://") || u.starts_with("https://") {
                    open_url_in_browser(u);
                }
            });
        }

        // 8h. Watch trailer search in browser
        {
            ui.on_watch_trailer(move |_hash, title| {
                watch_trailer(&title);
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

        // 12. Toggle Only Video files & Sort torrents
        {
            let shared = Arc::clone(&self.shared);
            ui.on_toggle_only_video(move |only| {
                shared.update_ts_config(|c| c.only_video_files = only);
                push_torrents_to_ui(&shared);
            });
        }
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_set_torrents_sort(move |mode| {
                shared.update_ts_config(|c| c.torrents_sort_mode = mode);
                if let Some(app) = weak.upgrade() {
                    app.set_torrents_sort_mode(mode);
                }
                push_torrents_to_ui(&shared);
            });
        }
        {
            let shared = Arc::clone(&self.shared);
            ui.on_set_torrents_filter(move |mode| {
                shared.filter_mode.store(mode, Ordering::Relaxed);
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
                    let api = TorrServerApi::from_config(&s.ts_config());
                    if let Err(e) = api.echo() {
                        let msg = format!(
                            "TorrServer не запущен ({e}). Сначала скачайте или запустите движок."
                        );
                        diagnostics::warn("[TS:API]", msg.clone());
                        s.toast(msg);
                        return;
                    }

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
                            Ok(t) => {
                                ok_count += 1;
                                let h = if !t.hash.trim().is_empty() {
                                    t.hash.clone()
                                } else {
                                    item.hash.clone()
                                };
                                let disp = if !t.display_title().trim().is_empty() && t.display_title() != h {
                                    t.display_title()
                                } else {
                                    item.title.clone()
                                };
                                let mag_opt = if !item.magnet.trim().is_empty() {
                                    Some(item.magnet.as_str())
                                } else {
                                    None
                                };
                                let ts = record_torrent_timestamp(&s, &h, t.timestamp);
                                record_torrent_origin(&s, &h, mag_opt, None, None, None, Some(&disp));
                                record_saved_torrent(
                                    &s,
                                    &h,
                                    &disp,
                                    mag_opt,
                                    None,
                                    if !t.poster.is_empty() { &t.poster } else { &item.poster },
                                    if !t.data.is_empty() { &t.data } else { &item.data },
                                    ts,
                                );
                            }
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

        // 13. Search torrents & Top 24h
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
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_fetch_top_24h(move || {
                if let Some(app) = weak.upgrade() {
                    app.set_search_loading(true);
                    app.set_search_error("".into());
                }
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    run_top_24h(&s);
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
            ui.on_add_search_result(move |magnet, title, page_url| {
                let m = magnet.to_string();
                let t = title.to_string();
                let u = page_url.trim().to_string();
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    let page_opt = if u.is_empty() { None } else { Some(u.as_str()) };
                    add_and_optionally_play(&s, &m, Some(&t), page_opt, false);
                });
            });
        }
        {
            let shared = Arc::clone(&self.shared);
            ui.on_play_search_result(move |magnet, title, page_url| {
                let m = magnet.to_string();
                let t = title.to_string();
                let u = page_url.trim().to_string();
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    let page_opt = if u.is_empty() { None } else { Some(u.as_str()) };
                    add_and_optionally_play(&s, &m, Some(&t), page_opt, true);
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
                        add_and_optionally_play(&s, &m, None, None, true);
                    } else {
                        let fallback_magnet = format!("magnet:?xt=urn:btih:{h}");
                        add_and_optionally_play(&s, &fallback_magnet, None, None, true);
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
                    let watchdog = app.get_engine_watchdog();
                    let keep_on_exit = app.get_keep_engine_on_exit();
                    let kp_key = app.get_kp_api_key().trim().to_string();

                    app.set_is_remote_mode(mode == 1);
                    shared.update_ts_config(|c| {
                        c.mode = mode;
                        c.port = port;
                        c.remote_url = remote_url;
                        c.username = username;
                        c.password = password;
                        c.engine_watchdog = watchdog;
                        c.keep_engine_on_exit = keep_on_exit;
                        c.kinopoisk_api_key = kp_key;
                    });
                    let updated_cfg = shared.ts_config();
                    apply_addresses_to_window(&updated_cfg, &app);
                    let s = Arc::clone(&shared);
                    thread::spawn(move || {
                        let cfg = s.ts_config();
                        let high_prio = s.keep_in_memory();
                        if cfg.mode == 0 {
                            reset_server_sync_state(&s);
                            let _ = s.server.restart(&cfg, high_prio);
                        } else {
                            reset_server_sync_state(&s);
                            s.server.stop(&cfg);
                        }
                        thread::sleep(Duration::from_millis(400));
                        refresh_torrents_and_status(&s);
                        let sel_h = s.selected_hash.lock().map(|g| g.clone()).unwrap_or_default();
                        if !sel_h.is_empty() {
                            load_torrent_details_async(&s, &sel_h, !cfg.kinopoisk_api_key.is_empty());
                        }
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
                    let high_prio = s.keep_in_memory();
                    reset_server_sync_state(&s);
                    match s.server.restart(&cfg, high_prio) {
                        Ok(_) => s.toast("Движок TorrServer перезапущен"),
                        Err(e) => s.toast(e),
                    }
                    thread::sleep(Duration::from_millis(500));
                    refresh_torrents_and_status(&s);
                });
            });
        }

        // 18a. Engine release selection & refresh
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_select_engine_version(move |idx| {
                let tag = if idx <= 0 {
                    String::new()
                } else {
                    shared
                        .available_releases
                        .lock()
                        .ok()
                        .and_then(|list| list.get((idx - 1) as usize).map(|r| r.tag.clone()))
                        .unwrap_or_default()
                };
                shared.update_ts_config(|c| c.preferred_engine_tag = tag);
                if let Some(app) = weak.upgrade() {
                    app.set_engine_version_selected_idx(idx.max(0));
                }
            });
        }
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_refresh_engine_versions(move || {
                if let Some(app) = weak.upgrade() {
                    app.set_engine_versions_loading(true);
                }
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    refresh_github_releases_bg(&s, true);
                });
            });
        }

        // 19. Download / Update engine from GitHub
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_download_update_engine(move || {
                let selected_idx = weak
                    .upgrade()
                    .map(|app| app.get_engine_version_selected_idx())
                    .unwrap_or(0);
                let target_tag: Option<String> = if selected_idx <= 0 {
                    None
                } else {
                    shared
                        .available_releases
                        .lock()
                        .ok()
                        .and_then(|list| list.get((selected_idx - 1) as usize).map(|r| r.tag.clone()))
                };

                if let Some(app) = weak.upgrade() {
                    app.set_engine_updating(true);
                    app.set_engine_update_progress(0.0);
                    app.set_engine_update_status("Стадия 1/4: подготовка к загрузке...".into());
                }
                shared.engine_updating.store(true, Ordering::Relaxed);
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    let cfg = s.ts_config();
                    let high_prio = s.keep_in_memory();
                    let known = s
                        .available_releases
                        .lock()
                        .map(|r| r.clone())
                        .unwrap_or_default();
                    let s_cb = Arc::clone(&s);
                    let res = s.server.update_from_github(
                        &cfg,
                        high_prio,
                        target_tag.as_deref(),
                        &known,
                        move |status_msg, prog| {
                            let msg = status_msg.to_string();
                            if let Ok(guard) = s_cb.ui_weak.lock() {
                                if let Some(w) = guard.as_ref() {
                                    let w_c = w.clone();
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(app) = w_c.upgrade() {
                                            app.set_engine_update_status(msg.into());
                                            app.set_engine_update_progress(prog);
                                        }
                                    });
                                }
                            }
                        },
                    );
                    s.engine_updating.store(false, Ordering::Relaxed);

                    let final_exists = s.server.binary_exists();
                    if let Ok(guard) = s.ui_weak.lock() {
                        if let Some(w) = guard.as_ref() {
                            let w_c = w.clone();
                            let msg = match &res {
                                Ok(tag) => format!("Установлен TorrServer {tag}"),
                                Err(e) => {
                                    let err_txt = format!("Ошибка обновления: {e}");
                                    diagnostics::error("[TS:UPDATER]", err_txt.clone());
                                    err_txt
                                }
                            };
                            let ok = res.is_ok();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(app) = w_c.upgrade() {
                                    app.set_engine_updating(false);
                                    app.set_engine_missing(!final_exists);
                                    app.set_engine_update_progress(if ok { 1.0 } else { 0.0 });
                                    app.set_engine_update_status(msg.clone().into());
                                    show_toast(&app, msg);
                                }
                            });
                            if ok {
                                reset_server_sync_state(&s);
                                thread::sleep(Duration::from_millis(600));
                                refresh_torrents_and_status(&s);
                            }
                        }
                    }
                });
            });
        }

        // 19a. Pick local TorrServer .exe manually
        {
            let shared = Arc::clone(&self.shared);
            ui.on_pick_engine_exe(move || {
                let s = Arc::clone(&shared);
                thread::spawn(move || {
                    let Some(path) = rfd::FileDialog::new()
                        .add_filter("TorrServer (.exe)", &["exe"])
                        .pick_file()
                    else {
                        return;
                    };
                    let cfg = s.ts_config();
                    let high_prio = s.keep_in_memory();
                    reset_server_sync_state(&s);
                    match s.server.install_local_exe(&cfg, &path, high_prio) {
                        Ok(_) => {
                            let final_exists = s.server.binary_exists();
                            if let Ok(guard) = s.ui_weak.lock() {
                                if let Some(w) = guard.as_ref() {
                                    let w_c = w.clone();
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(app) = w_c.upgrade() {
                                            app.set_engine_missing(!final_exists);
                                            app.set_engine_update_status(
                                                "Локальный движок установлен и запущен".into(),
                                            );
                                            show_toast(
                                                &app,
                                                "Движок TorrServer установлен и запущен",
                                            );
                                        }
                                    });
                                }
                            }
                            thread::sleep(Duration::from_millis(600));
                            refresh_torrents_and_status(&s);
                        }
                        Err(e) => {
                            diagnostics::error("[TS:ENGINE]", e.clone());
                            s.toast(e);
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
                open_url_in_browser(&url);
            });
        }

        // 20a. Copy Browser / Lampa URLs & refresh LAN IP
        {
            let shared = Arc::clone(&self.shared);
            ui.on_copy_ts_browser_url(move || {
                let (browser_url, _, _) = resolve_ts_addresses(&shared.ts_config());
                match arboard::Clipboard::new() {
                    Ok(mut cb) => {
                        if cb.set_text(browser_url.clone()).is_ok() {
                            shared.toast(format!("Адрес скопирован: {browser_url}"));
                        } else {
                            shared.toast("Не удалось записать адрес в буфер обмена");
                        }
                    }
                    Err(_) => {
                        shared.toast("Буфер обмена недоступен");
                    }
                }
            });
        }
        {
            let shared = Arc::clone(&self.shared);
            ui.on_copy_ts_lan_url(move || {
                let cfg = shared.ts_config();
                let (_, lan_url, has_lan) = resolve_ts_addresses(&cfg);
                if !has_lan {
                    sync_addresses_to_ui(&shared);
                    shared.toast("Локальный IP сети не определён — проверьте подключение к Wi-Fi/LAN");
                    return;
                }
                match arboard::Clipboard::new() {
                    Ok(mut cb) => {
                        if cb.set_text(lan_url.clone()).is_ok() {
                            shared.toast(format!("Адрес для Lampa скопирован: {lan_url}"));
                        } else {
                            shared.toast("Не удалось записать адрес в буфер обмена");
                        }
                    }
                    Err(_) => {
                        shared.toast("Буфер обмена недоступен");
                    }
                }
            });
        }
        {
            let shared = Arc::clone(&self.shared);
            ui.on_refresh_ts_addresses(move || {
                sync_addresses_to_ui(&shared);
                let (browser_url, lan_url, has_lan) = resolve_ts_addresses(&shared.ts_config());
                if has_lan {
                    shared.toast(format!("Адреса обновлены: {lan_url}"));
                } else {
                    shared.toast(format!("Адреса обновлены: {browser_url}"));
                }
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
                    show_toast(&app, "Поиск видеоплееров завершён");
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
                    show_toast(&app, "Настройки плеера и буфера сохранены");
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
                    let use_webview = app.get_rt_use_webview();
                    let mirror = app.get_rt_mirror().trim().to_string();
                    let username = app.get_rt_username().trim().to_string();
                    let password = app.get_rt_password().to_string();
                    let proxy = app.get_rt_proxy().trim().to_string();
                    let cookie = app.get_rt_cookie().trim().to_string();
                    let user_agent = app.get_rt_user_agent().replace(['\r', '\n'], " ").trim().to_string();

                    shared.update_ts_config(|c| {
                        c.rutracker_enabled = enabled;
                        c.rutracker_use_webview = use_webview;
                        c.rutracker_mirror = if mirror.is_empty() {
                            "https://rutracker.org".to_string()
                        } else {
                            mirror
                        };
                        c.rutracker_username = username;
                        c.rutracker_password = password;
                        c.rutracker_proxy = proxy;
                        c.rutracker_cookie = cookie;
                        c.rutracker_user_agent = user_agent;
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
                                        show_toast(&app, if ok {
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

        // 26. Open RuTracker WebView2 Login / Cloudflare Window
        {
            let shared = Arc::clone(&self.shared);
            let weak = ui.as_weak();
            ui.on_open_rutracker_webview(move || {
                if let Some(app) = weak.upgrade() {
                    let mirror = app.get_rt_mirror().trim().to_string();
                    let proxy = app.get_rt_proxy().trim().to_string();
                    let user_agent = app.get_rt_user_agent().replace(['\r', '\n'], " ").trim().to_string();

                    app.set_rt_enabled(true);
                    app.set_rt_use_webview(true);
                    app.set_rt_auth_status("Открытие окна встроенного браузера WebView2...".into());

                    shared.update_ts_config(|c| {
                        c.rutracker_enabled = true;
                        c.rutracker_use_webview = true;
                        c.rutracker_mirror = if mirror.is_empty() {
                            "https://rutracker.org".to_string()
                        } else {
                            mirror
                        };
                        c.rutracker_proxy = proxy;
                        c.rutracker_user_agent = user_agent;
                    });

                    let s = Arc::clone(&shared);
                    let w_outer = weak.clone();
                    thread::spawn(move || {
                        let cfg = s.ts_config();
                        let w_cb = Arc::new(Mutex::new(w_outer.clone()));
                        let res = s.rutracker.open_webview_login(&cfg, move |status_msg| {
                            let Some(w_inner) = w_cb.lock().ok().map(|g| g.clone()) else { return };
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(app) = w_inner.upgrade() {
                                    app.set_rt_auth_status(status_msg.clone().into());
                                    show_toast(&app, status_msg);
                                }
                            });
                        });
                        if let Err(e) = res {
                            diagnostics::error("[TS:RUTRACKER]", e.clone());
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(app) = w_outer.upgrade() {
                                    app.set_rt_auth_status(e.clone().into());
                                    show_toast(&app, e);
                                }
                            });
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
            sync_addresses_to_ui(&self.shared);
            let s = Arc::clone(&self.shared);
            thread::spawn(move || {
                refresh_torrents_and_status(&s);
            });
        }
    }

    fn on_shutdown(&mut self) {
        self.shared.poll_stop.store(true, Ordering::Relaxed);
        self.shared.rutracker.shutdown();
        if let Ok(mut guard) = self.shared.clipboard.lock() {
            if let Some(watcher) = guard.take() {
                watcher.stop();
            }
        }
        let cfg = self.shared.ts_config();
        if cfg.keep_engine_on_exit {
            diagnostics::info(
                "[TS:ENGINE]",
                "Движок MatriX оставлен запущенным в фоне (keep_engine_on_exit=true)",
            );
        } else {
            self.shared.server.stop(&cfg);
        }
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
            "  Mode: {} | Endpoint: {} | BinaryExists: {} | Watchdog: {} | KeepOnExit: {} | DetectedPlayers: {}",
            mode_str,
            cfg.effective_base_url(),
            self.shared.server.binary_exists(),
            cfg.engine_watchdog,
            cfg.keep_engine_on_exit,
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
                    let high_prio = s.keep_in_memory();
                    reset_server_sync_state(&s);
                    let _ = s.server.restart(&cfg, high_prio);
                    thread::sleep(Duration::from_millis(500));
                    refresh_torrents_and_status(&s);
                });
                true
            }
            "open_web_panel" => {
                let url = self.shared.ts_config().effective_base_url();
                open_url_in_browser(&url);
                true
            }
            "priority_high" => {
                self.shared.server.apply_child_priority(true);
                true
            }
            "priority_normal" => {
                self.shared.server.apply_child_priority(false);
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
    page_url: Option<&str>,
    auto_play: bool,
) {
    let cfg = shared.ts_config();
    let api = TorrServerApi::from_config(&cfg);

    let mut extracted_comment_url: Option<String> = None;
    let mut rt_topic_id: Option<String> = None;
    let mut uploaded_bytes_copy: Option<Vec<u8>> = None;
    let mut saved_magnet: Option<String> = if link.starts_with("magnet:") {
        Some(link.to_string())
    } else {
        None
    };

    let result = if let Some(topic_id) = link.strip_prefix("rutracker://topic/") {
        rt_topic_id = Some(topic_id.to_string());
        shared.toast("Загрузка торрента с RuTracker...");
        match shared.rutracker.fetch_torrent_or_magnet(&cfg, topic_id) {
            Ok(RuTrackerPayload::TorrentBytes { bytes, filename }) => {
                extracted_comment_url = extract_torrent_comment_url(&bytes);
                uploaded_bytes_copy = Some(bytes.clone());
                api.upload_torrent_bytes(bytes, filename)
            }
            Ok(RuTrackerPayload::Magnet(magnet)) => {
                saved_magnet = Some(magnet.clone());
                api.add_magnet(&magnet, title)
            }
            Err(e) => Err(e),
        }
    } else {
        api.add_magnet(link, title)
    };

    match result {
        Ok(t) => {
            let hash = if !t.hash.trim().is_empty() {
                t.hash.clone()
            } else {
                saved_magnet
                    .as_deref()
                    .and_then(extract_btih)
                    .unwrap_or_default()
            };
            let disp = if !t.display_title().trim().is_empty() && t.display_title() != hash {
                t.display_title()
            } else if let Some(custom) = title.filter(|s| !s.trim().is_empty()) {
                custom.trim().to_string()
            } else {
                hash.clone()
            };
            let ts = record_torrent_timestamp(shared, &hash, t.timestamp);
            let final_tracker_url = page_url
                .map(|s| s.to_string())
                .or(extracted_comment_url)
                .or_else(|| {
                    rt_topic_id
                        .as_ref()
                        .map(|tid| format!("{}/forum/viewtopic.php?t={tid}", cfg.effective_rutracker_mirror()))
                });
            record_torrent_origin(
                shared,
                &hash,
                saved_magnet.as_deref(),
                final_tracker_url.as_deref(),
                rt_topic_id.as_ref().map(|_| "RuTracker"),
                rt_topic_id.as_deref(),
                title.or(Some(disp.as_str())),
            );
            let saved_torrent_file = uploaded_bytes_copy
                .as_deref()
                .and_then(|bytes| save_torrent_bytes_to_disk(&shared.paths.ts_torrents_dir, &hash, bytes));
            record_saved_torrent(
                shared,
                &hash,
                &disp,
                saved_magnet.as_deref(),
                saved_torrent_file.as_deref(),
                &t.poster,
                &t.data,
                ts,
            );
            diagnostics::info("[TS:API]", format!("Добавлена раздача: {disp} ({hash})"));
            if let Ok(mut sel) = shared.selected_hash.lock() {
                *sel = hash.clone();
            }
            shared.toast(format!("Добавлено: {disp}"));
            refresh_torrents_and_status(shared);
            load_torrent_details_async(shared, &hash, false);
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
    let cfg = shared.ts_config();
    let api = TorrServerApi::from_config(&cfg);
    let mut last_hash = String::new();
    for p in paths {
        let bytes = match std::fs::read(p) {
            Ok(b) => b,
            Err(e) => {
                let msg = format!("Не удалось прочитать .torrent файл: {e}");
                diagnostics::error("[TS:API]", msg.clone());
                shared.toast(msg);
                continue;
            }
        };
        let comment_url = extract_torrent_comment_url(&bytes);
        let filename = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("upload.torrent")
            .to_string();
        let file_stem = p
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let file_rt_id = extract_rutracker_topic_id_from_filename(&filename);
        let effective_tracker_url = comment_url.or_else(|| {
            file_rt_id.as_ref().map(|tid| {
                format!(
                    "{}/forum/viewtopic.php?t={tid}",
                    cfg.effective_rutracker_mirror()
                )
            })
        });
        let bytes_copy = bytes.clone();
        match api.upload_torrent_bytes(bytes, filename) {
            Ok(t) => {
                let disp = t.display_title();
                let ts = record_torrent_timestamp(shared, &t.hash, t.timestamp);
                record_torrent_origin(
                    shared,
                    &t.hash,
                    None,
                    effective_tracker_url.as_deref(),
                    file_rt_id.as_ref().map(|_| "RuTracker"),
                    file_rt_id.as_deref(),
                    (!file_stem.is_empty()).then_some(file_stem.as_str()),
                );
                let saved_torrent_file =
                    save_torrent_bytes_to_disk(&shared.paths.ts_torrents_dir, &t.hash, &bytes_copy);
                record_saved_torrent(
                    shared,
                    &t.hash,
                    &disp,
                    None,
                    saved_torrent_file.as_deref(),
                    &t.poster,
                    &t.data,
                    ts,
                );
                diagnostics::info("[TS:API]", format!("Загружен .torrent: {disp}"));
                if let Ok(mut sel) = shared.selected_hash.lock() {
                    *sel = t.hash.clone();
                }
                last_hash = t.hash.clone();
                shared.toast(format!("Загружен: {disp}"));
            }
            Err(e) => {
                diagnostics::error("[TS:API]", e.clone());
                shared.toast(e);
            }
        }
    }
    refresh_torrents_and_status(shared);
    if !last_hash.is_empty() {
        load_torrent_details_async(shared, &last_hash, false);
    }
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

    let player_res = resolve_or_rescan_player(shared, &cfg);

    let launched = match player_res {
        Ok(exe) => launch_player(&exe, &stream_url, &cfg.player_args),
        Err(e) if cfg.player_custom_path.trim().is_empty() => {
            // Last-resort Windows shell fallback: write 1-track .m3u so OS default media app (e.g. Win11 UWP Media Player) opens the stream
            let single_key = format!("{}_{}", hash.trim(), file.id);
            match write_m3u_playlist(
                &shared.paths.ts_playlists_dir,
                &single_key,
                &[(base_name.to_string(), stream_url.clone())],
            ) {
                Ok(m3u_path) => open_with_os_default(&m3u_path),
                Err(_) => Err(e),
            }
        }
        Err(e) => Err(e),
    };

    match launched {
        Ok(_) => {
            let magnet = resolve_best_magnet(shared, hash);
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

    let player_res = resolve_or_rescan_player(shared, &cfg);

    let launched = match player_res {
        Ok(exe) => {
            let target = m3u_path.to_string_lossy().to_string();
            launch_player(&exe, &target, &cfg.player_args)
        }
        Err(_e) if cfg.player_custom_path.trim().is_empty() => open_with_os_default(&m3u_path),
        Err(e) => Err(e),
    };

    match launched {
        Ok(_) => {
            shared.toast(format!("Плейлист из {} серий открыт в плеере", entries.len()));
        }
        Err(e) => {
            diagnostics::error("[TS:PLAYER]", e.clone());
            shared.toast(e);
        }
    }
}

fn resolve_or_rescan_player(
    shared: &Arc<TorrServerShared>,
    cfg: &TorrServerConfig,
) -> Result<PathBuf, String> {
    let mut detected = shared
        .detected_players
        .lock()
        .map(|d| d.clone())
        .unwrap_or_default();

    if detected.is_empty() && cfg.player_custom_path.trim().is_empty() {
        let rescanned = detect_installed_players();
        if !rescanned.is_empty() {
            if let Ok(mut guard) = shared.detected_players.lock() {
                *guard = rescanned.clone();
            }
            detected = rescanned;
        }
    }

    let idx = detected
        .iter()
        .position(|p| p.id == cfg.player_id)
        .unwrap_or(0);
    resolve_player_exe(&cfg.player_custom_path, idx, &detected)
}

fn maybe_run_watchdog(shared: &Arc<TorrServerShared>, consec_echo_fail: &mut u32) {
    let cfg = shared.ts_config();
    if cfg.mode != 0
        || !cfg.engine_watchdog
        || shared.engine_updating.load(Ordering::Relaxed)
        || !shared.server.binary_exists()
    {
        *consec_echo_fail = 0;
        return;
    }

    let child_state = shared.server.child_running_state();
    let api = TorrServerApi::from_config(&cfg);
    match api.echo() {
        Ok(_) => {
            *consec_echo_fail = 0;
        }
        Err(err) => {
            if err.contains("HTTP 404") {
                *consec_echo_fail = 0;
                return;
            }
            *consec_echo_fail = consec_echo_fail.saturating_add(1);
            let exited = child_state == Some(false);
            if exited || *consec_echo_fail >= 3 {
                *consec_echo_fail = 0;
                diagnostics::warn(
                    "[TS:WATCHDOG]",
                    "Локальный движок TorrServer выгрузился или не отвечает — автоматический перезапуск",
                );
                let high_prio = shared.keep_in_memory();
                reset_server_sync_state(shared);
                if let Err(e) = shared.server.start(&cfg, high_prio) {
                    diagnostics::error("[TS:WATCHDOG]", format!("Не удалось перезапустить движок: {e}"));
                } else {
                    thread::sleep(Duration::from_millis(500));
                    refresh_torrents_and_status(shared);
                }
            }
        }
    }
}

fn load_torrents_meta(path: &Path) -> HashMap<String, i64> {
    if !path.exists() {
        return HashMap::new();
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<HashMap<String, i64>>(&raw).ok())
        .unwrap_or_default()
}

fn save_torrents_meta(path: &Path, map: &HashMap<String, i64>) {
    if let Ok(json) = serde_json::to_string_pretty(map) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            if path.exists() {
                let _ = std::fs::remove_file(path);
            }
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

fn record_torrent_timestamp(shared: &Arc<TorrServerShared>, hash: &str, server_ts: i64) -> i64 {
    let key = hash.trim().to_lowercase();
    if key.is_empty() {
        return server_ts;
    }
    let now = Utc::now().timestamp();
    let ts_candidate = if server_ts > 900_000_000 { server_ts } else { now };
    if let Ok(mut meta) = shared.torrents_meta.lock() {
        let mut changed = false;
        let val = match meta.get(&key).copied() {
            Some(existing) if server_ts > 900_000_000 && server_ts != existing => {
                meta.insert(key, server_ts);
                changed = true;
                server_ts
            }
            Some(existing) => existing,
            None => {
                meta.insert(key, ts_candidate);
                changed = true;
                ts_candidate
            }
        };
        if changed {
            save_torrents_meta(&shared.paths.ts_torrents_meta_path, &meta);
        }
        return val;
    }
    ts_candidate
}

fn sync_torrents_meta_from_list(shared: &Arc<TorrServerShared>, list: &[ApiTorrent]) {
    let now = Utc::now().timestamp();
    if let Ok(mut meta) = shared.torrents_meta.lock() {
        let mut changed = false;
        let total = list.len() as i64;
        for (idx, t) in list.iter().enumerate() {
            let key = t.hash.trim().to_lowercase();
            if key.is_empty() {
                continue;
            }
            if t.timestamp > 900_000_000 {
                if meta.get(&key).copied() != Some(t.timestamp) {
                    meta.insert(key, t.timestamp);
                    changed = true;
                }
            } else if !meta.contains_key(&key) {
                // Preserve initial server list order slightly spaced if multiple unseen items appear together
                meta.insert(key, now - (total - idx as i64));
                changed = true;
            }
        }
        if changed {
            save_torrents_meta(&shared.paths.ts_torrents_meta_path, &meta);
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

            let _sync_guard = shared.restore_lock.lock();
            match api.list_torrents() {
                Ok(mut list) => {
                    reconcile_torrents_with_server(shared, &api, &mut list);
                    let total_speed: f64 = list.iter().map(|t| t.download_speed).sum();
                    status.speed_label = format_speed(total_speed);
                    status.active_count = list.len() as i32;
                    sync_torrents_meta_from_list(shared, &list);
                    if let Ok(mut cache) = shared.cached_torrents.lock() {
                        *cache = list;
                    }
                }
                Err(e) => {
                    let should_log = shared
                        .last_list_err_log
                        .lock()
                        .ok()
                        .map(|mut guard| {
                            let now = Instant::now();
                            let log_it = guard
                                .map(|prev| now.duration_since(prev) > Duration::from_secs(15))
                                .unwrap_or(true);
                            if log_it {
                                *guard = Some(now);
                            }
                            log_it
                        })
                        .unwrap_or(false);
                    if should_log {
                        diagnostics::warn("[TS:API]", format!("Ошибка получения /torrents list: {e}"));
                    }
                }
            }
        }
        Err(e) => {
            status.healthy = false;
            status.state_label = if cfg.mode == 0 && !shared.server.binary_exists() {
                "Движок не установлен".to_string()
            } else if e.contains("HTTP 404") {
                "Несовместимый движок Silo (нужен MatriX)".to_string()
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
    let mut torrents = shared
        .cached_torrents
        .lock()
        .map(|c| c.clone())
        .unwrap_or_default();
    let meta_map = shared
        .torrents_meta
        .lock()
        .map(|m| m.clone())
        .unwrap_or_default();

    let ts_for = |t: &ApiTorrent| -> i64 {
        if t.timestamp > 900_000_000 {
            t.timestamp
        } else {
            meta_map
                .get(&t.hash.trim().to_lowercase())
                .copied()
                .unwrap_or(0)
        }
    };

    match cfg.torrents_sort_mode {
        1 => torrents.sort_by(|a, b| {
            ts_for(a)
                .cmp(&ts_for(b))
                .then_with(|| a.display_title().to_lowercase().cmp(&b.display_title().to_lowercase()))
        }),
        2 => torrents.sort_by(|a, b| {
            a.display_title()
                .to_lowercase()
                .cmp(&b.display_title().to_lowercase())
        }),
        3 => torrents.sort_by(|a, b| {
            b.torrent_size
                .cmp(&a.torrent_size)
                .then_with(|| ts_for(b).cmp(&ts_for(a)))
        }),
        4 => torrents.sort_by(|a, b| {
            b.download_speed
                .partial_cmp(&a.download_speed)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.active_peers.cmp(&a.active_peers))
                .then_with(|| ts_for(b).cmp(&ts_for(a)))
        }),
        _ => torrents.sort_by(|a, b| {
            ts_for(b)
                .cmp(&ts_for(a))
                .then_with(|| a.display_title().to_lowercase().cmp(&b.display_title().to_lowercase()))
        }),
    }

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
    let links_map = shared
        .torrents_links
        .lock()
        .map(|l| l.clone())
        .unwrap_or_default();
    let active_filter = shared.filter_mode.load(Ordering::Relaxed);

    let ui_torrents: Vec<TorrentItem> = torrents
        .iter()
        .filter(|t| matches_filter(t, active_filter))
        .map(|t| {
            let added_ts = ts_for(t);
            let added_text = format_unix_datetime_short(added_ts);
            build_ui_torrent_item(t, added_text, &selected, &links_map)
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
            ui_files.push(build_ui_file_item(
                f.id,
                idx as i32,
                file_name,
                f.path,
                f.length,
                stream,
                is_vid,
                watched_ids.contains(&f.id),
            ));
        }
    }

    if let Ok(guard) = shared.ui_weak.lock() {
        if let Some(weak) = guard.as_ref() {
            let w_c = weak.clone();
            let sel_hash = selected;
            let sort_mode = cfg.torrents_sort_mode;
            let torrents_snapshot = torrents;
            let ts_cfg_snap = cfg;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = w_c.upgrade() {
                    update_telemetry_and_counters(
                        &app,
                        &torrents_snapshot,
                        &sel_hash,
                        &ts_cfg_snap,
                        status.healthy,
                    );
                    app.set_torrents(ModelRc::from(Rc::new(VecModel::from(ui_torrents))));
                    app.set_torrents_sort_mode(sort_mode);
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

fn run_top_24h(shared: &Arc<TorrServerShared>) {
    let cfg = shared.ts_config();
    let api = TorrServerApi::from_config(&cfg);
    let source_mode = shared.search_source.lock().map(|m| *m).unwrap_or(0);

    let mut combined: Vec<ApiSearchResult> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    match source_mode {
        // 1: Only RuTor
        1 => match api.fetch_top_24h_rutor_direct() {
            Ok(list) => combined = list,
            Err(e) => notes.push(e),
        },
        // 2: Only RuTracker
        2 => match shared.rutracker.fetch_top_24h(&cfg) {
            Ok(list) => combined = list,
            Err(e) => notes.push(e),
        },
        // 3: Only JacRed
        3 => match api.fetch_top_24h_jacred() {
            Ok(list) => combined = list,
            Err(e) => notes.push(e),
        },
        // 0: All available sources combined
        _ => {
            match api.fetch_top_24h_jacred() {
                Ok(mut jr) => combined.append(&mut jr),
                Err(e) => notes.push(format!("JacRed: {e}")),
            }
            match api.fetch_top_24h_rutor_direct() {
                Ok(mut rt_top) => combined.append(&mut rt_top),
                Err(e) => notes.push(format!("RuTor: {e}")),
            }
            if cfg.rutracker_enabled && cfg.has_rutracker_credentials() {
                match shared.rutracker.fetch_top_24h(&cfg) {
                    Ok(mut rt) => combined.append(&mut rt),
                    Err(e) => notes.push(format!("RuTracker: {e}")),
                }
            }
        }
    }

    // Deduplicate by infohash or normalized title, keeping highest seeders
    combined.sort_by(|a, b| b.seeds.cmp(&a.seeds));
    let mut seen: HashSet<String> = HashSet::new();
    let mut deduped: Vec<ApiSearchResult> = Vec::with_capacity(combined.len());
    for item in combined {
        let key = extract_btih(&item.magnet)
            .unwrap_or_else(|| item.title.trim().to_lowercase());
        if seen.insert(key) {
            deduped.push(item);
        }
    }

    if !deduped.is_empty() {
        if let Ok(mut sort) = shared.search_sort.lock() {
            *sort = 0; // Popularity (by seeds desc)
        }
        if let Ok(mut raw) = shared.search_raw.lock() {
            *raw = deduped;
        }
        push_search_to_ui(shared, "");
        shared.toast("Загружен Топ популярных раздач за 24 часа");
    } else {
        if let Ok(mut raw) = shared.search_raw.lock() {
            raw.clear();
        }
        let err_text = if notes.is_empty() {
            "Не удалось получить список популярных раздач за 24 часа".to_string()
        } else {
            let msg = format!("Топ 24ч недоступен ({})", notes.join("; "));
            diagnostics::warn("[TS:API]", msg.clone());
            msg
        };
        push_search_to_ui(shared, &err_text);
    }
}

fn extract_btih(magnet: &str) -> Option<String> {
    let low = magnet.to_lowercase();
    let idx = low.find("xt=urn:btih:")?;
    let rest = &low[idx + "xt=urn:btih:".len()..];
    let hash: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    if hash.len() >= 16 {
        Some(hash)
    } else {
        None
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

    match sort_mode {
        1 => filtered.sort_by(|a, b| {
            b.date_ts
                .cmp(&a.date_ts)
                .then_with(|| b.seeds.cmp(&a.seeds))
        }),
        2 => filtered.sort_by(|a, b| {
            b.size_bytes
                .partial_cmp(&a.size_bytes)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.seeds.cmp(&a.seeds))
        }),
        3 => filtered.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
        _ => filtered.sort_by(|a, b| {
            b.seeds
                .cmp(&a.seeds)
                .then_with(|| b.date_ts.cmp(&a.date_ts))
        }),
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
            date_text: r.date_text.into(),
            page_url: r.page_url.into(),
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

fn refresh_github_releases_bg(shared: &Arc<TorrServerShared>, manual: bool) {
    match fetch_available_matrix_releases() {
        Ok(list) => {
            if let Ok(mut guard) = shared.available_releases.lock() {
                *guard = list;
            }
            if manual {
                shared.toast("Список версий MatriX обновлён с GitHub");
            }
        }
        Err(e) => {
            diagnostics::warn("[TS:UPDATER]", format!("Не удалось загрузить список версий MatriX: {e}"));
            if manual {
                shared.toast(format!("Не удалось обновить список версий: {e}"));
            }
        }
    }

    if let Ok(guard) = shared.ui_weak.lock() {
        if let Some(weak) = guard.as_ref() {
            let w_c = weak.clone();
            let s_c = Arc::clone(shared);
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = w_c.upgrade() {
                    app.set_engine_versions_loading(false);
                    sync_releases_to_ui(&s_c, &app);
                }
            });
        }
    }
}

fn sync_releases_to_ui(shared: &Arc<TorrServerShared>, ui: &AppWindow) {
    let cfg = shared.ts_config();
    let releases = shared
        .available_releases
        .lock()
        .map(|r| r.clone())
        .unwrap_or_default();

    let mut presets: Vec<SharedString> = Vec::with_capacity(releases.len() + 1);
    presets.push("Авто (последний стабильный MatriX)".into());
    for r in &releases {
        presets.push(r.label.clone().into());
    }

    let sel_idx = if cfg.preferred_engine_tag.trim().is_empty() {
        0
    } else {
        releases
            .iter()
            .position(|r| r.tag == cfg.preferred_engine_tag.trim())
            .map(|i| (i + 1) as i32)
            .unwrap_or(0)
    };

    ui.set_engine_version_presets(ModelRc::from(Rc::new(VecModel::from(presets))));
    ui.set_engine_version_selected_idx(sel_idx);
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
        presets.push("Системный плеер Windows (по умолчанию для .m3u)".into());
        ui.set_player_detect_status(
            "Внешний плеер не найден — при запуске используется системный обработчик Windows (.m3u) либо укажите .exe вручную."
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

fn load_torrents_links(path: &Path) -> HashMap<String, TorrentOriginMeta> {
    if !path.exists() {
        return HashMap::new();
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<HashMap<String, TorrentOriginMeta>>(&raw).ok())
        .unwrap_or_default()
}

fn save_torrents_links(path: &Path, map: &HashMap<String, TorrentOriginMeta>) {
    if let Ok(json) = serde_json::to_string_pretty(map) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            if path.exists() {
                let _ = std::fs::remove_file(path);
            }
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

fn load_torrents_saved(path: &Path) -> HashMap<String, SavedTorrentTask> {
    if !path.exists() {
        return HashMap::new();
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<HashMap<String, SavedTorrentTask>>(&raw).ok())
        .unwrap_or_default()
}

fn save_torrents_saved(path: &Path, map: &HashMap<String, SavedTorrentTask>) {
    if let Ok(json) = serde_json::to_string_pretty(map) {
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, json) {
            diagnostics::warn("[TS:SYNC]", format!("Не удалось записать {}: {e}", tmp.display()));
            return;
        }
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            diagnostics::warn("[TS:SYNC]", format!("Не удалось сохранить {}: {e}", path.display()));
        }
    }
}

fn build_placeholder_torrents(saved: &HashMap<String, SavedTorrentTask>) -> Vec<ApiTorrent> {
    let mut list: Vec<ApiTorrent> = saved
        .values()
        .filter(|t| !t.hash.trim().is_empty())
        .map(|t| ApiTorrent {
            hash: t.hash.clone(),
            title: t.title.clone(),
            timestamp: t.added_ts,
            stat_string: "Загрузка движка...".to_string(),
            poster: t.poster.clone(),
            data: t.data.clone(),
            ..Default::default()
        })
        .collect();
    list.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    list
}

fn save_torrent_bytes_to_disk(torrents_dir: &Path, hash: &str, bytes: &[u8]) -> Option<String> {
    let key = hash.trim().to_lowercase();
    if key.is_empty() || bytes.is_empty() {
        return None;
    }
    let filename = format!("{key}.torrent");
    let dest = torrents_dir.join(&filename);
    if std::fs::write(&dest, bytes).is_ok() {
        Some(filename)
    } else {
        None
    }
}

fn reset_server_sync_state(shared: &Arc<TorrServerShared>) {
    if let Ok(mut seen) = shared.server_seen_hashes.lock() {
        seen.clear();
    }
    shared.needs_server_restore.store(true, Ordering::SeqCst);
}

#[allow(clippy::too_many_arguments)]
fn record_saved_torrent(
    shared: &Arc<TorrServerShared>,
    hash: &str,
    title: &str,
    magnet: Option<&str>,
    torrent_file: Option<&str>,
    poster: &str,
    data: &str,
    added_ts: i64,
) {
    let key = hash.trim().to_lowercase();
    if key.is_empty() {
        return;
    }
    let now = Utc::now().timestamp();
    let best_magnet = magnet
        .map(str::trim)
        .filter(|m| m.starts_with("magnet:"))
        .map(|m| m.to_string())
        .unwrap_or_else(|| resolve_best_magnet(shared, hash));

    if let Ok(mut saved) = shared.saved_torrents.lock() {
        let entry = saved.entry(key.clone()).or_insert_with(|| SavedTorrentTask {
            hash: key.clone(),
            ..Default::default()
        });
        entry.hash = key.clone();
        if !title.trim().is_empty() && (entry.title.trim().is_empty() || title.trim() != key) {
            entry.title = title.trim().to_string();
        }
        if best_magnet.starts_with("magnet:")
            && (entry.magnet.is_empty() || best_magnet.len() > entry.magnet.len())
        {
            entry.magnet = best_magnet;
        }
        if let Some(tf) = torrent_file.map(str::trim).filter(|s| !s.is_empty()) {
            entry.torrent_file = tf.to_string();
        }
        if !poster.trim().is_empty() {
            entry.poster = poster.trim().to_string();
        }
        if !data.trim().is_empty() {
            entry.data = data.trim().to_string();
        }
        if entry.added_ts <= 900_000_000 {
            entry.added_ts = if added_ts > 900_000_000 { added_ts } else { now };
        }
        save_torrents_saved(&shared.paths.ts_torrents_saved_path, &saved);
    }

    if let Ok(mut seen) = shared.server_seen_hashes.lock() {
        seen.insert(key);
    }
}

fn remove_saved_torrent(shared: &Arc<TorrServerShared>, hash: &str) {
    let key = hash.trim().to_lowercase();
    if key.is_empty() {
        return;
    }
    let mut file_to_delete: Option<String> = None;
    if let Ok(mut saved) = shared.saved_torrents.lock() {
        if let Some(removed) = saved.remove(&key) {
            if !removed.torrent_file.trim().is_empty() {
                file_to_delete = Some(removed.torrent_file);
            }
            save_torrents_saved(&shared.paths.ts_torrents_saved_path, &saved);
        }
    }
    if let Ok(mut seen) = shared.server_seen_hashes.lock() {
        seen.remove(&key);
    }
    if let Some(fname) = file_to_delete {
        let p = shared.paths.ts_torrents_dir.join(fname);
        if p.exists() {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn reconcile_torrents_with_server(
    shared: &Arc<TorrServerShared>,
    api: &TorrServerApi,
    list: &mut Vec<ApiTorrent>,
) {

    if shared.needs_server_restore.load(Ordering::SeqCst) {
        let server_hashes: HashSet<String> = list
            .iter()
            .map(|t| t.hash.trim().to_lowercase())
            .filter(|h| !h.is_empty())
            .collect();

        let mut missing: Vec<SavedTorrentTask> = shared
            .saved_torrents
            .lock()
            .map(|saved| {
                saved
                    .values()
                    .filter(|task| {
                        let k = task.hash.trim().to_lowercase();
                        !k.is_empty() && !server_hashes.contains(&k)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        missing.sort_by_key(|t| t.added_ts);

        let mut restored_ok = 0usize;
        for task in missing {
            let file_res = if !task.torrent_file.trim().is_empty() {
                let p = shared.paths.ts_torrents_dir.join(task.torrent_file.trim());
                if p.exists() {
                    if let Ok(bytes) = std::fs::read(&p) {
                        Some(api.upload_torrent_bytes(bytes, task.torrent_file.clone()))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            let res = match file_res {
                Some(Ok(t)) => Ok(t),
                _ => {
                    let magnet = if !task.magnet.trim().is_empty() {
                        task.magnet.clone()
                    } else {
                        resolve_best_magnet(shared, &task.hash)
                    };
                    api.add_backup_item(&TorrentBackupItem {
                        hash: task.hash.clone(),
                        title: task.title.clone(),
                        poster: task.poster.clone(),
                        data: task.data.clone(),
                        magnet,
                    })
                }
            };

            match res {
                Ok(_) => restored_ok += 1,
                Err(e) => diagnostics::warn(
                    "[TS:SYNC]",
                    format!("Не удалось восстановить раздачу '{}': {e}", task.title),
                ),
            }
        }

        if restored_ok > 0 {
            diagnostics::info(
                "[TS:SYNC]",
                format!("Автоматически восстановлено раздач в TorrServer: {restored_ok}"),
            );
            if let Ok(updated) = api.list_torrents() {
                *list = updated;
            }
        }

        shared.needs_server_restore.store(false, Ordering::SeqCst);
        let new_seen: HashSet<String> = list
            .iter()
            .map(|t| t.hash.trim().to_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        if let Ok(mut seen) = shared.server_seen_hashes.lock() {
            *seen = new_seen;
        }
    }

    // Snapshot saved metadata/links safely without holding multiple locks simultaneously
    let links_snapshot = shared
        .torrents_links
        .lock()
        .map(|l| l.clone())
        .unwrap_or_default();
    let meta_snapshot = shared
        .torrents_meta
        .lock()
        .map(|m| m.clone())
        .unwrap_or_default();
    let prev_seen = shared
        .server_seen_hashes
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();

    let server_hashes: HashSet<String> = list
        .iter()
        .map(|t| t.hash.trim().to_lowercase())
        .filter(|h| !h.is_empty())
        .collect();

    let now = Utc::now().timestamp();
    let mut files_to_delete: Vec<String> = Vec::new();

    if let Ok(mut saved) = shared.saved_torrents.lock() {
        let mut changed = false;

        // Upsert active torrents from server into local persistent list
        for t in list.iter() {
            let key = t.hash.trim().to_lowercase();
            if key.is_empty() {
                continue;
            }
            let disp = t.display_title();
            let ts_cand = if t.timestamp > 900_000_000 {
                t.timestamp
            } else {
                meta_snapshot.get(&key).copied().unwrap_or(now)
            };
            let magnet_cand = links_snapshot
                .get(&key)
                .map(|o| o.magnet.trim().to_string())
                .filter(|m| m.starts_with("magnet:"))
                .unwrap_or_else(|| format!("magnet:?xt=urn:btih:{key}"));

            let is_new = !saved.contains_key(&key);
            let entry = saved.entry(key.clone()).or_insert_with(|| SavedTorrentTask {
                hash: key.clone(),
                ..Default::default()
            });
            if is_new {
                changed = true;
            }
            if !disp.trim().is_empty() && (entry.title.trim().is_empty() || (disp != key && entry.title == key)) {
                entry.title = disp;
                changed = true;
            }
            if magnet_cand.starts_with("magnet:")
                && (entry.magnet.is_empty() || magnet_cand.len() > entry.magnet.len())
            {
                entry.magnet = magnet_cand;
                changed = true;
            }
            if !t.poster.trim().is_empty() && entry.poster != t.poster.trim() {
                entry.poster = t.poster.trim().to_string();
                changed = true;
            }
            if !t.data.trim().is_empty() && entry.data != t.data.trim() {
                entry.data = t.data.trim().to_string();
                changed = true;
            }
            if entry.added_ts <= 900_000_000 && ts_cand > 900_000_000 {
                entry.added_ts = ts_cand;
                changed = true;
            }
        }

        // If a hash was confirmed on the server earlier in this active session and disappeared now
        // (e.g. deleted from TorrServer Web UI), remove it from saved_torrents as well.
        for gone_hash in prev_seen.difference(&server_hashes) {
            if let Some(removed) = saved.remove(gone_hash) {
                changed = true;
                if !removed.torrent_file.trim().is_empty() {
                    files_to_delete.push(removed.torrent_file);
                }
            }
        }

        if changed {
            save_torrents_saved(&shared.paths.ts_torrents_saved_path, &saved);
        }
    }

    if let Ok(mut seen) = shared.server_seen_hashes.lock() {
        *seen = server_hashes;
    }

    for fname in files_to_delete {
        let p = shared.paths.ts_torrents_dir.join(fname);
        if p.exists() {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn record_torrent_origin(
    shared: &Arc<TorrServerShared>,
    hash: &str,
    magnet: Option<&str>,
    tracker_url: Option<&str>,
    tracker_name: Option<&str>,
    rt_topic_id: Option<&str>,
    source_title: Option<&str>,
) {
    let key = hash.trim().to_lowercase();
    if key.is_empty() {
        return;
    }
    if let Ok(mut links) = shared.torrents_links.lock() {
        let entry = links.entry(key).or_default();
        let mut changed = false;
        if let Some(m) = magnet {
            let trimmed = m.trim();
            if trimmed.starts_with("magnet:") && entry.magnet != trimmed {
                entry.magnet = trimmed.to_string();
                changed = true;
            }
        }
        if let Some(u) = tracker_url {
            let trimmed = u.trim();
            if (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
                && entry.tracker_url != trimmed
            {
                entry.tracker_url = trimmed.to_string();
                changed = true;
            }
        }
        if let Some(tn) = tracker_name {
            if !tn.trim().is_empty() && entry.tracker_name != tn.trim() {
                entry.tracker_name = tn.trim().to_string();
                changed = true;
            }
        }
        if let Some(tid) = rt_topic_id {
            if !tid.trim().is_empty() && entry.rutracker_topic_id != tid.trim() {
                entry.rutracker_topic_id = tid.trim().to_string();
                changed = true;
            }
        }
        if let Some(st) = source_title {
            let trimmed = st.trim();
            if !trimmed.is_empty() && entry.source_title != trimmed {
                entry.source_title = trimmed.to_string();
                changed = true;
            }
        }
        if changed {
            save_torrents_links(&shared.paths.ts_torrents_links_path, &links);
        }
    }
}

fn resolve_best_magnet(shared: &Arc<TorrServerShared>, hash: &str) -> String {
    let key = hash.trim().to_lowercase();
    if let Ok(links) = shared.torrents_links.lock() {
        if let Some(meta) = links.get(&key) {
            if meta.magnet.starts_with("magnet:") {
                return meta.magnet.clone();
            }
        }
    }
    format!("magnet:?xt=urn:btih:{hash}")
}

fn resolve_tracker_url(
    shared: &Arc<TorrServerShared>,
    cfg: &TorrServerConfig,
    hash: &str,
) -> Option<String> {
    let key = hash.trim().to_lowercase();
    if let Ok(links) = shared.torrents_links.lock() {
        if let Some(meta) = links.get(&key) {
            if meta.tracker_url.starts_with("http://") || meta.tracker_url.starts_with("https://") {
                return Some(meta.tracker_url.clone());
            }
            if !meta.rutracker_topic_id.is_empty() {
                return Some(format!(
                    "{}/forum/viewtopic.php?t={}",
                    cfg.effective_rutracker_mirror(),
                    meta.rutracker_topic_id
                ));
            }
        }
    }
    if let Ok(cache) = shared.detail_meta_cache.lock() {
        if let Some(m) = cache.get(&key) {
            if m.tracker_url.starts_with("http://") || m.tracker_url.starts_with("https://") {
                return Some(m.tracker_url.clone());
            }
        }
    }
    None
}

fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    let trimmed = cleaned.trim().chars().take(80).collect::<String>();
    if trimmed.is_empty() {
        "playlist".to_string()
    } else {
        trimmed
    }
}

fn rgba_to_slint_image(img: &DecodedRgbaImage) -> Option<Image> {
    if img.width == 0 || img.height == 0 || img.rgba.len() != (img.width * img.height * 4) as usize {
        return None;
    }
    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&img.rgba, img.width, img.height);
    Some(Image::from_rgba8(buf))
}

fn clear_detail_ui(shared: &Arc<TorrServerShared>) {
    shared.detail_generation.fetch_add(1, Ordering::SeqCst);
    if let Ok(guard) = shared.ui_weak.lock() {
        if let Some(weak) = guard.as_ref() {
            let w_c = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = w_c.upgrade() {
                    app.set_detail_loading(false);
                    app.set_detail_title("".into());
                    app.set_detail_original_title("".into());
                    app.set_detail_year("".into());
                    app.set_detail_genres("".into());
                    app.set_detail_country("".into());
                    app.set_detail_rating_kp("".into());
                    app.set_detail_rating_imdb("".into());
                    app.set_detail_description("".into());
                    app.set_detail_source("".into());
                    app.set_detail_tracker_url("".into());
                    app.set_detail_has_poster(false);
                    app.set_detail_screenshots(ModelRc::from(Rc::new(VecModel::<ScreenshotItem>::default())));
                }
            });
        }
    }
}

fn load_torrent_details_async(shared: &Arc<TorrServerShared>, hash: &str, force_refresh: bool) {
    let target_hash = hash.trim().to_string();
    if target_hash.is_empty() {
        clear_detail_ui(shared);
        return;
    }
    let hash_key = target_hash.to_lowercase();
    let gen = shared.detail_generation.fetch_add(1, Ordering::SeqCst) + 1;

    let cached_t = shared
        .cached_torrents
        .lock()
        .ok()
        .and_then(|list| list.iter().find(|t| t.hash == target_hash).cloned());
    let initial_title = cached_t
        .as_ref()
        .map(|t| t.display_title())
        .unwrap_or_else(|| target_hash.clone());
    let tracker_url_hint = resolve_tracker_url(shared, &shared.ts_config(), &target_hash).unwrap_or_default();

    if let Ok(guard) = shared.ui_weak.lock() {
        if let Some(weak) = guard.as_ref() {
            let w_c = weak.clone();
            let t_init = initial_title;
            let tr_init = tracker_url_hint;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = w_c.upgrade() {
                    app.set_detail_loading(true);
                    app.set_detail_title(t_init.into());
                    app.set_detail_original_title("".into());
                    app.set_detail_year("".into());
                    app.set_detail_genres("".into());
                    app.set_detail_country("".into());
                    app.set_detail_rating_kp("".into());
                    app.set_detail_rating_imdb("".into());
                    app.set_detail_description("".into());
                    app.set_detail_source("".into());
                    app.set_detail_tracker_url(tr_init.into());
                    app.set_detail_has_poster(false);
                    app.set_detail_screenshots(ModelRc::from(Rc::new(VecModel::<ScreenshotItem>::default())));
                }
            });
        }
    }

    let s = Arc::clone(shared);
    thread::Builder::new()
        .name("ts-rich-meta".to_string())
        .spawn(move || {
            let cfg = s.ts_config();
            let torrent = match cached_t {
                Some(t) => t,
                None => TorrServerApi::from_config(&cfg)
                    .get_torrent(&target_hash)
                    .unwrap_or_else(|_| ApiTorrent {
                        hash: target_hash.clone(),
                        ..Default::default()
                    }),
            };
            let origin = s
                .torrents_links
                .lock()
                .ok()
                .and_then(|m| m.get(&hash_key).cloned());

            let payload = fetch_rich_metadata(
                &s.paths.ts_posters_dir,
                &cfg,
                &s.rutracker,
                &torrent,
                origin.as_ref(),
                force_refresh,
            );

            if !payload.meta.tracker_url.is_empty() {
                record_torrent_origin(
                    &s,
                    &target_hash,
                    None,
                    Some(&payload.meta.tracker_url),
                    None,
                    None,
                    None,
                );
            }
            if let Ok(mut cache) = s.detail_meta_cache.lock() {
                cache.insert(hash_key.clone(), payload.meta.clone());
            }

            if s.detail_generation.load(Ordering::SeqCst) != gen {
                return;
            }
            let still_selected = s
                .selected_hash
                .lock()
                .map(|sel| sel.eq_ignore_ascii_case(&target_hash))
                .unwrap_or(false);
            if !still_selected {
                return;
            }

            if let Ok(guard) = s.ui_weak.lock() {
                if let Some(weak) = guard.as_ref() {
                    let w_c = weak.clone();
                    let s_check = Arc::clone(&s);
                    let _ = slint::invoke_from_event_loop(move || {
                        if s_check.detail_generation.load(Ordering::SeqCst) != gen {
                            return;
                        }
                        if let Some(app) = w_c.upgrade() {
                            app.set_detail_loading(false);
                            app.set_detail_title(payload.meta.title.into());
                            app.set_detail_original_title(payload.meta.original_title.into());
                            app.set_detail_year(payload.meta.year.into());
                            app.set_detail_genres(payload.meta.genres.into());
                            app.set_detail_country(payload.meta.country.into());
                            app.set_detail_rating_kp(payload.meta.rating_kp.into());
                            app.set_detail_rating_imdb(payload.meta.rating_imdb.into());
                            app.set_detail_description(payload.meta.description.into());
                            app.set_detail_source(payload.meta.source_label.into());
                            app.set_detail_tracker_url(payload.meta.tracker_url.into());

                            if let Some(poster_rgba) = payload.poster.as_ref().and_then(rgba_to_slint_image) {
                                app.set_detail_poster(poster_rgba);
                                app.set_detail_has_poster(true);
                            } else {
                                app.set_detail_has_poster(false);
                            }

                            let shots: Vec<ScreenshotItem> = payload
                                .screenshots
                                .iter()
                                .filter_map(|d| {
                                    rgba_to_slint_image(d).map(|img| ScreenshotItem {
                                        img,
                                        url: d.url.clone().into(),
                                    })
                                })
                                .collect();
                            app.set_detail_screenshots(ModelRc::from(Rc::new(VecModel::from(shots))));
                        }
                    });
                }
            }
        })
        .ok();
}

fn detect_local_lan_ip() -> Option<String> {
    use std::net::{IpAddr, UdpSocket};
    let probes = [
        "8.8.8.8:80",
        "1.1.1.1:80",
        "192.168.1.1:80",
        "192.168.0.1:80",
        "10.0.0.1:80",
        "172.16.0.1:80",
    ];
    for target in probes {
        if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
            if sock.connect(target).is_ok() {
                if let Ok(local) = sock.local_addr() {
                    if let IpAddr::V4(v4) = local.ip() {
                        if !v4.is_loopback() && !v4.is_unspecified() && !v4.is_link_local() {
                            return Some(v4.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

fn resolve_ts_addresses(cfg: &TorrServerConfig) -> (String, String, bool) {
    let browser_url = cfg.effective_base_url();
    if cfg.mode == 1 {
        let has_remote = !browser_url.is_empty();
        (browser_url.clone(), browser_url, has_remote)
    } else if let Some(ip) = detect_local_lan_ip() {
        (browser_url, format!("http://{ip}:{}", cfg.port), true)
    } else {
        (
            browser_url,
            "Локальный IP не определён (проверьте сеть)".to_string(),
            false,
        )
    }
}

fn apply_addresses_to_window(cfg: &TorrServerConfig, app: &AppWindow) {
    let (browser_url, lan_url, has_lan) = resolve_ts_addresses(cfg);
    app.set_ts_browser_url(browser_url.into());
    app.set_ts_lan_url(lan_url.into());
    app.set_ts_has_lan_ip(has_lan);
}

fn sync_addresses_to_ui(shared: &Arc<TorrServerShared>) {
    let cfg = shared.ts_config();
    if let Ok(guard) = shared.ui_weak.lock() {
        if let Some(weak) = guard.as_ref() {
            let w_c = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = w_c.upgrade() {
                    apply_addresses_to_window(&cfg, &app);
                }
            });
        }
    }
}

