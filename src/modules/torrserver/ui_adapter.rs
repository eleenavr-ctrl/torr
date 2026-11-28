use crate::core::storage::{TorrServerConfig, TorrentOriginMeta};
use crate::modules::torrserver::api::{format_bytes, format_speed, ApiTorrent};
use crate::platform::windows::open_url_in_browser;
use crate::{AppState, AppWindow, PeerItem, SpeedSample, TorrentFileItem, TorrentItem};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

static SPEED_RING: OnceLock<Mutex<VecDeque<f64>>> = OnceLock::new();

pub fn extract_quality_tag(title: &str) -> String {
    let lower = title.to_lowercase();
    let mut tags = Vec::new();
    if lower.contains("2160p") || lower.contains("4k") || lower.contains("uhd") {
        tags.push("4K");
    } else if lower.contains("1080p") || lower.contains("bdremux") {
        tags.push("1080p");
    } else if lower.contains("720p") {
        tags.push("720p");
    }
    if lower.contains("dolby vision") || lower.contains(" dovi") || lower.contains(".dv.") {
        tags.push("DV");
    } else if lower.contains("hdr") {
        tags.push("HDR");
    }
    tags.join(" ")
}

pub fn extract_codec_tag(filename: &str) -> String {
    let lower = filename.to_lowercase();
    if lower.contains("av1") {
        "AV1".into()
    } else if lower.contains("hevc") || lower.contains("x265") || lower.contains("h265") {
        "HEVC".into()
    } else if lower.contains("x264") || lower.contains("h264") || lower.contains("avc") {
        "H.264".into()
    } else if lower.contains("remux") {
        "Remux".into()
    } else {
        String::new()
    }
}

pub fn is_series_title(title: &str) -> bool {
    let lower = title.to_lowercase();
    lower.contains("сезон")
        || lower.contains("серии")
        || lower.contains("season")
        || lower.contains("s01")
        || lower.contains("s02")
        || lower.contains("s03")
        || lower.contains("из ")
}

pub fn is_4k_hdr(title: &str) -> bool {
    let lower = title.to_lowercase();
    lower.contains("2160p") || lower.contains("4k") || lower.contains("uhd") || lower.contains("hdr")
}

pub fn matches_filter(t: &ApiTorrent, filter_mode: i32) -> bool {
    let title = t.display_title();
    let is_active = t.download_speed > 1024.0 || t.upload_speed > 1024.0 || t.active_peers > 0;
    let is_completed = t.torrent_size > 0 && t.loaded_size >= t.torrent_size;
    match filter_mode {
        1 => is_active,
        2 => is_4k_hdr(&title),
        3 => is_series_title(&title),
        4 => is_completed,
        _ => true,
    }
}

pub fn build_ui_torrent_item(
    t: &ApiTorrent,
    added_text: String,
    selected_hash: &str,
    links: &HashMap<String, TorrentOriginMeta>,
) -> TorrentItem {
    let title = t.display_title();
    let progress = if t.torrent_size > 0 {
        (t.loaded_size as f64 / t.torrent_size as f64) as f32
    } else {
        0.0
    };
    let is_active = t.download_speed > 1024.0 || t.upload_speed > 1024.0 || t.active_peers > 0;
    let is_completed = t.torrent_size > 0 && t.loaded_size >= t.torrent_size;
    let tracker_tag = links
        .get(&t.hash.to_lowercase())
        .map(|o| o.tracker_name.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if t.category.trim().is_empty() {
                String::new()
            } else {
                t.category.clone()
            }
        });

    TorrentItem {
        hash: t.hash.clone().into(),
        title: title.clone().into(),
        size_text: format_bytes(t.torrent_size).into(),
        speed_text: format_speed(t.download_speed).into(),
        down_speed: format_speed(t.download_speed).into(),
        up_speed: format_speed(t.upload_speed).into(),
        cache_text: if t.loaded_size > 0 {
            format_bytes(t.loaded_size).into()
        } else {
            "".into()
        },
        peers_text: format!("{} / {}", t.active_peers, t.connected_seeders).into(),
        progress,
        status_text: if t.stat_string.is_empty() {
            "Активен".into()
        } else {
            t.stat_string.clone().into()
        },
        added_text: added_text.into(),
        quality_tag: extract_quality_tag(&title).into(),
        tracker_tag: tracker_tag.into(),
        is_active,
        is_series: is_series_title(&title),
        is_completed,
        magnet: format!("magnet:?xt=urn:btih:{}", t.hash).into(),
        is_selected: t.hash == selected_hash,
    }
}

pub fn build_ui_file_item(
    id: i32,
    index: i32,
    file_name: String,
    path: String,
    length: u64,
    stream: String,
    is_vid: bool,
    is_watched: bool,
) -> TorrentFileItem {
    let codec_tag = extract_codec_tag(&file_name);
    TorrentFileItem {
        id,
        index,
        name: file_name.into(),
        path: path.into(),
        size_text: format_bytes(length).into(),
        codec_tag: codec_tag.into(),
        stream_url: stream.into(),
        is_video: is_vid,
        is_watched,
    }
}

pub fn update_telemetry_and_counters(
    app: &AppWindow,
    all_torrents: &[ApiTorrent],
    selected_hash: &str,
    ts_cfg: &TorrServerConfig,
    healthy: bool,
) {
    let total_down: f64 = all_torrents.iter().map(|t| t.download_speed).sum();
    let total_up: f64 = all_torrents.iter().map(|t| t.upload_speed).sum();
    let total_peers: i32 = all_torrents.iter().map(|t| t.total_peers.max(t.active_peers)).sum();
    let active_t = all_torrents.iter().find(|t| t.hash == selected_hash).or_else(|| all_torrents.first());
    let buf_pct = active_t
        .map(|t| {
            if t.torrent_size > 0 {
                ((t.loaded_size as f64 / t.torrent_size as f64) * 100.0).clamp(0.0, 100.0) as i32
            } else {
                0
            }
        })
        .unwrap_or(0);

    let ring = SPEED_RING.get_or_init(|| Mutex::new(VecDeque::from(vec![0.0; 16])));
    let samples: Vec<SpeedSample> = if let Ok(mut guard) = ring.lock() {
        if guard.len() >= 16 {
            guard.pop_front();
        }
        guard.push_back(total_down);
        let max_v = guard.iter().copied().fold(1024.0_f64, f64::max);
        guard
            .iter()
            .enumerate()
            .map(|(i, v)| SpeedSample {
                label: i.to_string().into(),
                ratio: (*v / max_v).clamp(0.0, 1.0) as f32,
            })
            .collect()
    } else {
        vec![]
    };

    let count_all = all_torrents.len() as i32;
    let count_active = all_torrents
        .iter()
        .filter(|t| t.download_speed > 1024.0 || t.upload_speed > 1024.0 || t.active_peers > 0)
        .count() as i32;
    let count_4k = all_torrents
        .iter()
        .filter(|t| is_4k_hdr(&t.display_title()))
        .count() as i32;
    let count_series = all_torrents
        .iter()
        .filter(|t| is_series_title(&t.display_title()))
        .count() as i32;
    let count_completed = all_torrents
        .iter()
        .filter(|t| t.torrent_size > 0 && t.loaded_size >= t.torrent_size)
        .count() as i32;

    let sel_peers: Vec<PeerItem> = if let Some(sel) = active_t {
        if sel.active_peers > 0 || sel.connected_seeders > 0 {
            vec![
                PeerItem {
                    addr: "Active Swarm".into(),
                    client: "BitTorrent".into(),
                    progress: format!("{}/{} peers", sel.active_peers, sel.total_peers).into(),
                    speed: format_speed(sel.download_speed).into(),
                    flags: "D/E".into(),
                },
                PeerItem {
                    addr: "Seeders".into(),
                    client: "Tracker/DHT".into(),
                    progress: format!("{} connected", sel.connected_seeders).into(),
                    speed: format_speed(sel.upload_speed).into(),
                    flags: "U".into(),
                },
            ]
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    let state = app.global::<AppState>();
    state.set_telemetry_down_speed(format_speed(total_down).into());
    state.set_telemetry_up_speed(format_speed(total_up).into());
    state.set_telemetry_cache_usage(format!("{} / {} МБ", buf_pct, ts_cfg.cache_size_mb).into());
    state.set_telemetry_buffer_pct(buf_pct);
    state.set_telemetry_ping(if healthy { "OK".into() } else { "—".into() });
    state.set_telemetry_dht(format!("DHT: {total_peers}").into());

    state.set_count_all(count_all);
    state.set_count_active(count_active);
    state.set_count_4k(count_4k);
    state.set_count_series(count_series);
    state.set_count_completed(count_completed);
    state.set_detail_peers(ModelRc::from(Rc::new(VecModel::from(sel_peers))));
    state.set_detail_speed_history(ModelRc::from(Rc::new(VecModel::from(samples))));
}

pub fn watch_trailer(title: &str) {
    let cleaned = title.trim();
    if cleaned.is_empty() {
        return;
    }
    let query = format!("{cleaned} официальный трейлер");
    let url = format!(
        "https://www.youtube.com/results?search_query={}",
        urlencoding::encode(&query)
    );
    open_url_in_browser(&url);
}
