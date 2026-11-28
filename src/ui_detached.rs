use crate::platform::windows::theme::{apply_dwm_dark_mode, detect_windows_dark_mode};
use crate::platform::windows::window_hooks::extract_hwnd;
use crate::{AppWindow, DetachedWindow};
use slint::{CloseRequestResponse, ComponentHandle};
use std::cell::RefCell;
use std::rc::Rc;

pub fn bind_detach_callbacks(
    app: &AppWindow,
    icon: slint::Image,
    holder: Rc<RefCell<Option<DetachedWindow>>>,
) {
    let weak = app.as_weak();
    app.on_detach_module(move |tab| {
        if tab < 0 {
            if let Some(dw) = holder.borrow_mut().take() {
                let _ = dw.hide();
            }
            if let Some(a) = weak.upgrade() {
                a.set_detached_tab(-1);
            }
            return;
        }

        if let Some(prev) = holder.borrow_mut().take() {
            let _ = prev.hide();
        }
        let Some(a) = weak.upgrade() else {
            return;
        };
        a.set_detached_tab(tab);
        let Ok(dw) = DetachedWindow::new() else {
            a.set_detached_tab(-1);
            return;
        };
        dw.set_app_icon(icon.clone());
        dw.set_detached_tab(tab);
        let mode = a.get_theme_mode();
        let sys_dark = detect_windows_dark_mode();
        dw.invoke_sync_palette(mode, sys_dark);

        forward_callbacks(&dw, &weak);

        let w_dock = weak.clone();
        let h_dock = Rc::clone(&holder);
        dw.on_dock_back(move || {
            if let Some(w) = h_dock.borrow_mut().take() {
                let _ = w.hide();
            }
            if let Some(app) = w_dock.upgrade() {
                app.set_detached_tab(-1);
            }
        });

        let w_close = weak.clone();
        let h_close = Rc::clone(&holder);
        dw.window().on_close_requested(move || {
            h_close.borrow_mut().take();
            if let Some(app) = w_close.upgrade() {
                app.set_detached_tab(-1);
            }
            CloseRequestResponse::HideWindow
        });

        let _ = dw.show();
        let dw_hwnd = extract_hwnd(dw.window());
        let is_dark = mode == 2 || (mode == 0 && sys_dark);
        apply_dwm_dark_mode(dw_hwnd, is_dark);

        *holder.borrow_mut() = Some(dw);
    });
}

fn forward_callbacks(dw: &DetachedWindow, weak: &slint::Weak<AppWindow>) {
    macro_rules! fwd0 {
        ($dw_on:ident, $app_inv:ident) => {{
            let w = weak.clone();
            dw.$dw_on(move || {
                if let Some(a) = w.upgrade() {
                    a.$app_inv();
                }
            });
        }};
    }
    macro_rules! fwd1 {
        ($dw_on:ident, $app_inv:ident) => {{
            let w = weak.clone();
            dw.$dw_on(move |x| {
                if let Some(a) = w.upgrade() {
                    a.$app_inv(x);
                }
            });
        }};
    }
    macro_rules! fwd2 {
        ($dw_on:ident, $app_inv:ident) => {{
            let w = weak.clone();
            dw.$dw_on(move |x, y| {
                if let Some(a) = w.upgrade() {
                    a.$app_inv(x, y);
                }
            });
        }};
    }
    macro_rules! fwd3 {
        ($dw_on:ident, $app_inv:ident) => {{
            let w = weak.clone();
            dw.$dw_on(move |x, y, z| {
                if let Some(a) = w.upgrade() {
                    a.$app_inv(x, y, z);
                }
            });
        }};
    }

    fwd1!(on_add_magnet, invoke_add_magnet);
    fwd0!(on_paste_and_add_magnet, invoke_paste_and_add_magnet);
    fwd0!(on_pick_torrent_file, invoke_pick_torrent_file);
    fwd1!(on_select_torrent, invoke_select_torrent);
    fwd0!(on_refresh_torrents, invoke_refresh_torrents);
    fwd1!(on_set_torrents_sort, invoke_set_torrents_sort);
    fwd1!(on_set_torrents_filter, invoke_set_torrents_filter);
    fwd1!(on_drop_torrent, invoke_drop_torrent);
    fwd1!(on_remove_torrent, invoke_remove_torrent);
    fwd1!(on_copy_torrent_magnet, invoke_copy_torrent_magnet);
    fwd2!(on_play_file, invoke_play_file);
    fwd1!(on_select_engine_version, invoke_select_engine_version);
    fwd0!(on_refresh_engine_versions, invoke_refresh_engine_versions);
    fwd0!(on_download_update_engine, invoke_download_update_engine);
    fwd0!(on_pick_engine_exe, invoke_pick_engine_exe);
    fwd0!(on_export_torrents, invoke_export_torrents);
    fwd0!(on_import_torrents, invoke_import_torrents);
    fwd1!(on_download_torrent_playlist, invoke_download_torrent_playlist);
    fwd0!(on_download_all_playlist, invoke_download_all_playlist);
    fwd1!(on_refresh_single_torrent, invoke_refresh_single_torrent);
    fwd1!(on_show_torrent_info, invoke_show_torrent_info);
    fwd1!(on_open_tracker_page, invoke_open_tracker_page);

    fwd1!(on_search_torrents, invoke_search_torrents);
    fwd0!(on_fetch_top_24h, invoke_fetch_top_24h);
    fwd1!(on_set_search_quality, invoke_set_search_quality);
    fwd1!(on_set_search_sort, invoke_set_search_sort);
    fwd1!(on_set_search_source, invoke_set_search_source);
    fwd3!(on_add_search_result, invoke_add_search_result);
    fwd3!(on_play_search_result, invoke_play_search_result);

    fwd1!(on_toggle_history_favorites, invoke_toggle_history_favorites);
    fwd2!(on_play_history_item, invoke_play_history_item);
    fwd1!(on_toggle_history_favorite, invoke_toggle_history_favorite);
    fwd1!(on_remove_history_item, invoke_remove_history_item);

    fwd1!(on_ws_load_preset, invoke_ws_load_preset);
    fwd1!(on_ws_save_preset, invoke_ws_save_preset);
    fwd1!(on_ws_delete_preset, invoke_ws_delete_preset);
    fwd2!(on_ws_set_block_span, invoke_ws_set_block_span);
    fwd2!(on_ws_move_block, invoke_ws_move_block);
}
