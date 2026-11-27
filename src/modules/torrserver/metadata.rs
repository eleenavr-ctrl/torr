use crate::core::diagnostics;
use crate::core::storage::{TorrentOriginMeta, TorrServerConfig};
use crate::modules::torrserver::api::ApiTorrent;
use crate::modules::torrserver::rutracker::RuTrackerClient;
use chrono::Utc;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TorrentRichMetadata {
    pub hash: String,
    pub title: String,
    pub original_title: String,
    pub year: String,
    pub genres: String,
    pub country: String,
    pub rating_kp: String,
    pub rating_imdb: String,
    pub description: String,
    pub source_label: String,
    pub tracker_url: String,
    pub kp_id: String,
    pub imdb_id: String,
    pub poster_url: String,
    pub screenshot_urls: Vec<String>,
    pub fetched_at: i64,
}

#[derive(Debug, Clone)]
pub struct DecodedRgbaImage {
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct RichMediaPayload {
    pub meta: TorrentRichMetadata,
    pub poster: Option<DecodedRgbaImage>,
    pub screenshots: Vec<DecodedRgbaImage>,
}

pub fn fetch_rich_metadata(
    posters_dir: &Path,
    cfg: &TorrServerConfig,
    rutracker: &RuTrackerClient,
    torrent: &ApiTorrent,
    origin: Option<&TorrentOriginMeta>,
    force_refresh: bool,
) -> RichMediaPayload {
    let hash_key = torrent.hash.trim().to_lowercase();
    let cache_json_path = posters_dir.join(format!("{hash_key}.json"));
    let now = Utc::now().timestamp();

    if !force_refresh && cache_json_path.exists() {
        if let Ok(raw) = fs::read_to_string(&cache_json_path) {
            if let Ok(cached) = serde_json::from_str::<TorrentRichMetadata>(&raw) {
                let age = now - cached.fetched_at;
                let has_data = !cached.poster_url.is_empty() || !cached.description.is_empty();
                if age < 7 * 86_400 && has_data {
                    let mut payload = RichMediaPayload {
                        meta: cached,
                        poster: None,
                        screenshots: Vec::new(),
                    };
                    load_or_download_images(posters_dir, cfg, &mut payload);
                    return payload;
                }
            }
        }
    }

    let raw_title = torrent.display_title();
    let source_hint = origin
        .map(|o| o.source_title.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("");

    let (disp_ru, disp_en, disp_year) = extract_clean_title_and_year(&raw_title);
    let (hint_ru, hint_en, hint_year) = if !source_hint.is_empty() {
        extract_clean_title_and_year(source_hint)
    } else {
        (String::new(), None, None)
    };

    let clean_ru = if contains_cyrillic(&hint_ru) {
        hint_ru
    } else if contains_cyrillic(&disp_ru) || disp_ru.is_empty() {
        if !disp_ru.is_empty() {
            disp_ru
        } else {
            hint_ru
        }
    } else {
        disp_ru
    };
    let clean_en = disp_en.or(hint_en);
    let parsed_year = disp_year.or(hint_year);

    let mut meta = TorrentRichMetadata {
        hash: hash_key.clone(),
        title: if !clean_ru.is_empty() {
            clean_ru.clone()
        } else {
            raw_title.clone()
        },
        original_title: clean_en.clone().unwrap_or_default(),
        year: parsed_year.clone().unwrap_or_default(),
        fetched_at: now,
        ..Default::default()
    };

    if let Some(org) = origin {
        if !org.tracker_url.trim().is_empty() {
            meta.tracker_url = org.tracker_url.trim().to_string();
        }
    }

    // 1. Seed from TorrServer's own poster/data fields (Lampa / TorrServer metadata)
    extract_from_torrserver_fields(torrent, &mut meta);

    let http = build_http_client();

    // 2. Parse tracker page (RuTracker / RuTor) for Russian description, poster, real screenshots, KP/IMDb IDs
    let rt_topic_id = origin
        .and_then(|o| {
            if !o.rutracker_topic_id.is_empty() {
                Some(o.rutracker_topic_id.clone())
            } else {
                extract_rutracker_topic_id(&o.tracker_url)
            }
        })
        .or_else(|| extract_rutracker_topic_id(&meta.tracker_url))
        .or_else(|| extract_rutracker_topic_id_from_filename(source_hint));

    if let Some(tid) = rt_topic_id {
        if meta.tracker_url.is_empty() {
            meta.tracker_url = format!(
                "{}/forum/viewtopic.php?t={tid}",
                cfg.effective_rutracker_mirror()
            );
        }
        if !cfg.rutracker_use_webview || (cfg.rutracker_enabled && cfg.has_rutracker_credentials()) {
            match rutracker.fetch_topic_html(cfg, &tid) {
                Ok(html) => {
                    parse_rutracker_topic_html(&html, &mut meta);
                    if meta.source_label.is_empty() {
                        meta.source_label = "RuTracker".to_string();
                    }
                }
                Err(e) => {
                    diagnostics::info(
                        "[TS:META]",
                        format!(
                            "RuTracker тема #{tid} пропущена ({e}) — используются IMDb / Wikipedia / Кинопоиск"
                        ),
                    );
                }
            }
        }
    } else if meta.tracker_url.to_lowercase().contains("rutor.") {
        if let Ok(html) = fetch_text_url(&http, &meta.tracker_url, None) {
            parse_rutor_topic_html(&html, &mut meta);
            if meta.source_label.is_empty() {
                meta.source_label = "RuTor".to_string();
            }
        }
    }

    // 3. Kinopoisk Unofficial API (optional free key)
    let kp_key = cfg.kinopoisk_api_key.trim();
    if !kp_key.is_empty()
        && (meta.poster_url.is_empty()
            || meta.description.is_empty()
            || meta.rating_kp.is_empty()
            || meta.screenshot_urls.len() < 2)
    {
        let query = if !clean_ru.is_empty() {
            clean_ru.as_str()
        } else {
            raw_title.as_str()
        };
        enrich_from_kinopoisk(&http, kp_key, query, parsed_year.as_deref(), &mut meta);
    }

    // 4. IMDb (no API key needed) + Wikidata/RuWikipedia for Russian summary if needed
    let needs_imdb = meta.poster_url.is_empty()
        || meta.description.is_empty()
        || meta.rating_imdb.is_empty()
        || meta.screenshot_urls.is_empty();
    if needs_imdb {
        let mut owned_candidates: Vec<String> = Vec::new();
        if let Some(en) = clean_en.as_deref() {
            if !en.is_empty() {
                owned_candidates.push(en.to_string());
                let spaced = split_camel_case(en);
                if spaced != en && !spaced.is_empty() {
                    owned_candidates.push(spaced);
                }
            }
        }
        if !clean_ru.is_empty() && !owned_candidates.iter().any(|c| c == &clean_ru) {
            owned_candidates.push(clean_ru.clone());
        }
        if owned_candidates.is_empty() {
            owned_candidates.push(raw_title.clone());
        }
        let cand_refs: Vec<&str> = owned_candidates.iter().map(|s| s.as_str()).collect();
        enrich_from_imdb(&http, &cand_refs, parsed_year.as_deref(), &mut meta);
    }

    if meta.description.is_empty()
        || !contains_cyrillic(&meta.description)
        || meta.poster_url.is_empty()
    {
        let imdb_id_copy = meta.imdb_id.clone();
        enrich_russian_summary_from_wikipedia(
            &http,
            &imdb_id_copy,
            &clean_ru,
            clean_en.as_deref(),
            parsed_year.as_deref(),
            &mut meta,
        );
    }

    meta.fetched_at = Utc::now().timestamp();
    if meta.source_label.is_empty() {
        meta.source_label = if !meta.poster_url.is_empty() || !meta.description.is_empty() {
            "IMDb / Веб".to_string()
        } else {
            "TorrServer".to_string()
        };
    }

    if let Ok(json) = serde_json::to_string_pretty(&meta) {
        let _ = fs::write(&cache_json_path, json);
    }

    let mut payload = RichMediaPayload {
        meta,
        poster: None,
        screenshots: Vec::new(),
    };
    load_or_download_images(posters_dir, cfg, &mut payload);
    payload
}

fn build_http_client() -> Client {
    Client::builder()
        .connect_timeout(Duration::from_secs(7))
        .timeout(Duration::from_secs(10))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
        .build()
        .unwrap_or_else(|_| Client::new())
}

fn fetch_text_url(client: &Client, url: &str, referer: Option<&str>) -> Result<String, String> {
    let mut req = client
        .get(url)
        .header("Accept-Language", "ru-RU,ru;q=0.9,en-US;q=0.8,en;q=0.7");
    if let Some(r) = referer {
        req = req.header("Referer", r);
    }
    let resp = req.send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    resp.text().map_err(|e| e.to_string())
}

fn load_or_download_images(
    posters_dir: &Path,
    cfg: &TorrServerConfig,
    payload: &mut RichMediaPayload,
) {
    let hash_key = payload.meta.hash.clone();
    let referer = if payload.meta.tracker_url.is_empty() {
        None
    } else {
        Some(payload.meta.tracker_url.clone())
    };
    let client = build_http_client();

    if !payload.meta.poster_url.is_empty() {
        let poster_path = posters_dir.join(format!("{hash_key}_poster.img"));
        if let Some(decoded) = fetch_cached_or_remote_image(
            &client,
            &poster_path,
            &payload.meta.poster_url,
            referer.as_deref(),
            cfg,
            420,
        ) {
            payload.poster = Some(decoded);
        }
    }

    let shots: Vec<String> = payload
        .meta
        .screenshot_urls
        .iter()
        .take(3)
        .cloned()
        .collect();
    for (idx, url) in shots.into_iter().enumerate() {
        let shot_path = posters_dir.join(format!("{hash_key}_shot_{idx}.img"));
        if let Some(decoded) = fetch_cached_or_remote_image(
            &client,
            &shot_path,
            &url,
            referer.as_deref(),
            cfg,
            460,
        ) {
            payload.screenshots.push(decoded);
        }
    }
}

fn fetch_cached_or_remote_image(
    client: &Client,
    cache_path: &Path,
    url: &str,
    referer: Option<&str>,
    _cfg: &TorrServerConfig,
    max_dim: u32,
) -> Option<DecodedRgbaImage> {
    if cache_path.exists() {
        if let Ok(bytes) = fs::read(cache_path) {
            if let Some(img) = decode_image_bytes(url, &bytes, max_dim) {
                return Some(img);
            }
            let _ = fs::remove_file(cache_path);
        }
    }

    let bytes = download_image_bytes_with_viewer_fallback(client, url, referer, 0)?;
    let decoded = decode_image_bytes(url, &bytes, max_dim)?;
    let _ = fs::write(cache_path, &bytes);
    Some(decoded)
}

fn download_image_bytes_with_viewer_fallback(
    client: &Client,
    url: &str,
    referer: Option<&str>,
    depth: u8,
) -> Option<Vec<u8>> {
    let trimmed = url.trim();
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return None;
    }
    let mut req = client
        .get(trimmed)
        .header("Accept", "image/avif,image/webp,image/apng,image/*,*/*;q=0.8");
    if let Some(ref_url) = referer {
        req = req.header("Referer", ref_url);
    } else if let Some(origin) = url_origin(trimmed) {
        req = req.header("Referer", origin);
    }

    let resp = req.send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    let bytes = resp.bytes().ok()?.to_vec();
    if bytes.is_empty() || bytes.len() > 15 * 1024 * 1024 {
        return None;
    }

    if ct.contains("text/html") && depth == 0 {
        // FastPic / ImageBan HTML wrapper page -> extract real <img> direct URL
        let html = String::from_utf8_lossy(&bytes);
        if let Some(real_url) = extract_direct_image_from_viewer_html(&html, trimmed) {
            return download_image_bytes_with_viewer_fallback(
                client,
                &real_url,
                Some(trimmed),
                1,
            );
        }
        return None;
    }

    Some(bytes)
}

fn extract_direct_image_from_viewer_html(html: &str, page_url: &str) -> Option<String> {
    let origin = url_origin(page_url).unwrap_or_default();
    for marker in [
        "class=\"image img-fluid\"",
        "id=\"image\"",
        "class=\"pic\"",
        "property=\"og:image\"",
    ] {
        if let Some(pos) = html.find(marker) {
            let start = pos.saturating_sub(140);
            let end = (pos + 260).min(html.len());
            let slice = &html[start..end];
            for attr in ["src=\"", "content=\"", "src='", "content='"] {
                if let Some(a_pos) = slice.find(attr) {
                    let quote = if attr.ends_with('\'') { '\'' } else { '"' };
                    let rest = &slice[a_pos + attr.len()..];
                    if let Some(q_end) = rest.find(quote) {
                        let cand = rest[..q_end].trim();
                        if !cand.is_empty() && !cand.contains("logo") && !cand.contains("favicon") {
                            if cand.starts_with("http") {
                                return Some(cand.to_string());
                            } else if cand.starts_with("//") {
                                return Some(format!("https:{cand}"));
                            } else if cand.starts_with('/') && !origin.is_empty() {
                                return Some(format!("{origin}{cand}"));
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn decode_image_bytes(url: &str, bytes: &[u8], max_dim: u32) -> Option<DecodedRgbaImage> {
    let dyn_img = image::load_from_memory(bytes).ok()?;
    let resized = if dyn_img.width() > max_dim || dyn_img.height() > max_dim {
        dyn_img.thumbnail(max_dim, max_dim)
    } else {
        dyn_img
    };
    let rgba = resized.to_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    Some(DecodedRgbaImage {
        url: url.to_string(),
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

fn extract_from_torrserver_fields(torrent: &ApiTorrent, meta: &mut TorrentRichMetadata) {
    if !torrent.poster.trim().is_empty() && torrent.poster.trim().starts_with("http") {
        meta.poster_url = torrent.poster.trim().to_string();
    }
    if torrent.data.trim().is_empty() {
        return;
    }
    let Ok(v) = serde_json::from_str::<Value>(&torrent.data) else {
        return;
    };

    let candidate_obj = v
        .get("movie")
        .or_else(|| v.get("TorrServer"))
        .unwrap_or(&v);

    if meta.poster_url.is_empty() {
        for k in ["poster", "poster_path", "img", "backdrop_path"] {
            if let Some(s) = candidate_obj.get(k).and_then(|x| x.as_str()) {
                let st = s.trim();
                if st.starts_with("http") {
                    meta.poster_url = st.to_string();
                    break;
                } else if st.starts_with('/') && st.ends_with(".jpg") {
                    meta.poster_url = format!("https://image.tmdb.org/t/p/w500{st}");
                    break;
                }
            }
        }
    }

    if meta.description.is_empty() {
        for k in ["overview", "description", "plot"] {
            if let Some(s) = candidate_obj.get(k).and_then(|x| x.as_str()) {
                let st = s.trim();
                if !st.is_empty() {
                    meta.description = st.to_string();
                    meta.source_label = "TorrServer".to_string();
                    break;
                }
            }
        }
    }

    if let Some(imdb) = candidate_obj.get("imdb_id").and_then(|x| x.as_str()) {
        if imdb.starts_with("tt") {
            meta.imdb_id = imdb.to_string();
        }
    }
    if let Some(kp) = candidate_obj.get("kp_id").and_then(|x| x.as_i64()) {
        if kp > 0 {
            meta.kp_id = kp.to_string();
        }
    }
    if let Some(vote) = candidate_obj.get("vote_average").and_then(|x| x.as_f64()) {
        if vote > 0.0 && meta.rating_imdb.is_empty() {
            meta.rating_imdb = format!("{vote:.1}");
        }
    }
}

fn parse_rutracker_topic_html(html: &str, meta: &mut TorrentRichMetadata) {
    let scope = if let Some(idx) = html.find("class=\"post_body\"") {
        let tail = &html[idx..];
        let end = tail.find("class=\"sp-head\"").unwrap_or(tail.len().min(32000));
        let full_end = tail.find("<div class=\"clear\"").unwrap_or(tail.len().min(48000));
        let _ = end;
        &tail[..full_end]
    } else {
        html
    };

    if meta.kp_id.is_empty() {
        meta.kp_id = extract_kp_id_from_text(scope).unwrap_or_default();
    }
    if meta.imdb_id.is_empty() {
        meta.imdb_id = extract_imdb_id_from_text(scope).unwrap_or_default();
    }

    // Collect postImg images
    let mut post_imgs: Vec<String> = Vec::new();
    let mut scan = 0;
    while let Some(rel) = scope[scan..].find("class=\"postImg") {
        let start = scan + rel;
        let tag_end = scope[start..].find('>').map(|e| start + e).unwrap_or(start);
        let attrs = &scope[start..tag_end];
        if let Some(url) = extract_attr_value(attrs, "title=\"")
            .or_else(|| extract_attr_value(attrs, "src=\""))
        {
            if !is_junk_tracker_image(&url) && !post_imgs.contains(&url) {
                post_imgs.push(url);
            }
        }
        scan = tag_end + 1;
        if post_imgs.len() >= 12 {
            break;
        }
    }

    if meta.poster_url.is_empty() {
        if let Some(first_img) = post_imgs.first() {
            meta.poster_url = first_img.clone();
        }
    }

    if meta.screenshot_urls.is_empty() {
        // Prefer images inside spoiler blocks if present, otherwise remaining postImg entries
        let spoiler_imgs = extract_images_from_spoiler_blocks(scope);
        if !spoiler_imgs.is_empty() {
            for u in spoiler_imgs {
                if Some(&u) != post_imgs.first() && !meta.screenshot_urls.contains(&u) {
                    meta.screenshot_urls.push(u);
                }
                if meta.screenshot_urls.len() >= 4 {
                    break;
                }
            }
        }
        if meta.screenshot_urls.is_empty() && post_imgs.len() > 1 {
            for u in post_imgs.into_iter().skip(1) {
                if !meta.screenshot_urls.contains(&u) {
                    meta.screenshot_urls.push(u);
                }
                if meta.screenshot_urls.len() >= 4 {
                    break;
                }
            }
        }
    }

    let plain = html_to_lines(scope);
    for line in &plain {
        let low = line.to_lowercase();
        if meta.year.is_empty() && (low.starts_with("год выпуска:") || low.starts_with("год:")) {
            if let Some(y) = line.split(':').nth(1) {
                meta.year = y.trim().chars().take(4).collect();
            }
        } else if meta.genres.is_empty() && low.starts_with("жанр:") {
            if let Some(g) = line.split(':').nth(1) {
                meta.genres = g.trim().to_string();
            }
        } else if meta.country.is_empty() && low.starts_with("страна:") {
            if let Some(c) = line.split(':').nth(1) {
                meta.country = c.trim().to_string();
            }
        } else if meta.description.is_empty()
            && (low.starts_with("описание:")
                || low.starts_with("сюжет:")
                || low.starts_with("о фильме:"))
        {
            if let Some((_, rest)) = line.split_once(':') {
                let d = rest.trim();
                if d.len() > 25 {
                    meta.description = d.to_string();
                }
            }
        }
    }
}

fn parse_rutor_topic_html(html: &str, meta: &mut TorrentRichMetadata) {
    if meta.kp_id.is_empty() {
        meta.kp_id = extract_kp_id_from_text(html).unwrap_or_default();
    }
    if meta.imdb_id.is_empty() {
        meta.imdb_id = extract_imdb_id_from_text(html).unwrap_or_default();
    }

    let scope = if let Some(idx) = html.find("id=\"details\"") {
        let tail = &html[idx..];
        let end = tail.find("id=\"comments\"").unwrap_or(tail.len().min(32000));
        &tail[..end]
    } else {
        &html[..html.len().min(32000)]
    };

    let mut imgs: Vec<String> = Vec::new();
    let mut scan = 0;
    while let Some(rel) = scope[scan..].find("<img ") {
        let start = scan + rel;
        let end = scope[start..].find('>').map(|e| start + e).unwrap_or(start);
        let attrs = &scope[start..end];
        if let Some(src) = extract_attr_value(attrs, "src=\"")
            .or_else(|| extract_attr_value(attrs, "src='"))
        {
            if (src.starts_with("http://") || src.starts_with("https://"))
                && !is_junk_tracker_image(&src)
                && !imgs.contains(&src)
            {
                imgs.push(src);
            }
        }
        scan = end + 1;
        if imgs.len() >= 10 {
            break;
        }
    }

    if meta.poster_url.is_empty() {
        if let Some(first) = imgs.first() {
            meta.poster_url = first.clone();
        }
    }
    if meta.screenshot_urls.is_empty() && imgs.len() > 1 {
        meta.screenshot_urls = imgs.into_iter().skip(1).take(4).collect();
    }

    for line in html_to_lines(scope) {
        let low = line.to_lowercase();
        if meta.description.is_empty()
            && (low.starts_with("описание:") || low.starts_with("о фильме:"))
        {
            if let Some((_, rest)) = line.split_once(':') {
                let d = rest.trim();
                if d.len() > 25 {
                    meta.description = d.to_string();
                }
            }
        } else if meta.genres.is_empty() && low.starts_with("жанр:") {
            if let Some((_, rest)) = line.split_once(':') {
                meta.genres = rest.trim().to_string();
            }
        }
    }
}

fn enrich_from_kinopoisk(
    client: &Client,
    api_key: &str,
    query: &str,
    year: Option<&str>,
    meta: &mut TorrentRichMetadata,
) {
    let mut kp_id = meta.kp_id.trim().to_string();
    if kp_id.is_empty() {
        let encoded = urlencoding::encode(query.trim());
        let search_url = format!(
            "https://kinopoiskapiunofficial.tech/api/v2.1/films/search-by-keyword?keyword={encoded}"
        );
        if let Ok(resp) = client
            .get(&search_url)
            .header("X-API-KEY", api_key)
            .header("Accept", "application/json")
            .send()
        {
            if resp.status().is_success() {
                if let Ok(v) = resp.json::<Value>() {
                    if let Some(arr) = v.get("films").and_then(|x| x.as_array()) {
                        let picked = year
                            .and_then(|y| {
                                arr.iter().find(|f| {
                                    f.get("year")
                                        .and_then(|fy| fy.as_str())
                                        .map(|s| s == y)
                                        .unwrap_or(false)
                                })
                            })
                            .or_else(|| arr.first());
                        if let Some(film) = picked {
                            if let Some(id) = film.get("filmId").and_then(|x| x.as_i64()) {
                                kp_id = id.to_string();
                                meta.kp_id = kp_id.clone();
                            }
                        }
                    }
                }
            }
        }
    }

    if kp_id.is_empty() {
        return;
    }

    let detail_url = format!("https://kinopoiskapiunofficial.tech/api/v2.2/films/{kp_id}");
    if let Ok(resp) = client
        .get(&detail_url)
        .header("X-API-KEY", api_key)
        .header("Accept", "application/json")
        .send()
    {
        if resp.status().is_success() {
            if let Ok(f) = resp.json::<Value>() {
                if let Some(ru) = f.get("nameRu").and_then(|x| x.as_str()) {
                    if !ru.trim().is_empty() {
                        meta.title = ru.trim().to_string();
                    }
                }
                if meta.original_title.is_empty() {
                    if let Some(orig) = f
                        .get("nameOriginal")
                        .or_else(|| f.get("nameEn"))
                        .and_then(|x| x.as_str())
                    {
                        meta.original_title = orig.trim().to_string();
                    }
                }
                if meta.year.is_empty() {
                    if let Some(y) = f.get("year").and_then(|x| x.as_i64()) {
                        meta.year = y.to_string();
                    }
                }
                if let Some(r_kp) = f.get("ratingKinopoisk").and_then(|x| x.as_f64()) {
                    if r_kp > 0.0 {
                        meta.rating_kp = format!("{r_kp:.1}");
                    }
                }
                if let Some(r_imdb) = f.get("ratingImdb").and_then(|x| x.as_f64()) {
                    if r_imdb > 0.0 && meta.rating_imdb.is_empty() {
                        meta.rating_imdb = format!("{r_imdb:.1}");
                    }
                }
                if meta.imdb_id.is_empty() {
                    if let Some(imdb) = f.get("imdbId").and_then(|x| x.as_str()) {
                        if imdb.starts_with("tt") {
                            meta.imdb_id = imdb.to_string();
                        }
                    }
                }
                if meta.genres.is_empty() {
                    if let Some(arr) = f.get("genres").and_then(|x| x.as_array()) {
                        let list: Vec<String> = arr
                            .iter()
                            .filter_map(|g| g.get("genre").and_then(|s| s.as_str()))
                            .map(|s| s.trim().to_string())
                            .take(4)
                            .collect();
                        meta.genres = list.join(", ");
                    }
                }
                if meta.country.is_empty() {
                    if let Some(arr) = f.get("countries").and_then(|x| x.as_array()) {
                        let list: Vec<String> = arr
                            .iter()
                            .filter_map(|c| c.get("country").and_then(|s| s.as_str()))
                            .map(|s| s.trim().to_string())
                            .take(2)
                            .collect();
                        meta.country = list.join(", ");
                    }
                }
                if meta.description.is_empty() || !contains_cyrillic(&meta.description) {
                    if let Some(desc) = f
                        .get("description")
                        .or_else(|| f.get("shortDescription"))
                        .and_then(|x| x.as_str())
                    {
                        if !desc.trim().is_empty() {
                            meta.description = desc.trim().to_string();
                        }
                    }
                }
                if meta.poster_url.is_empty() {
                    if let Some(p) = f
                        .get("posterUrlPreview")
                        .or_else(|| f.get("posterUrl"))
                        .and_then(|x| x.as_str())
                    {
                        if p.starts_with("http") {
                            meta.poster_url = p.to_string();
                        }
                    }
                }
                meta.source_label = if meta.source_label.is_empty() {
                    "Кинопоиск".to_string()
                } else {
                    format!("{} + Кинопоиск", meta.source_label)
                };
            }
        }
    }

    if meta.screenshot_urls.len() < 2 {
        let img_url = format!(
            "https://kinopoiskapiunofficial.tech/api/v2.2/films/{kp_id}/images?type=STILL&page=1"
        );
        if let Ok(resp) = client
            .get(&img_url)
            .header("X-API-KEY", api_key)
            .header("Accept", "application/json")
            .send()
        {
            if resp.status().is_success() {
                if let Ok(v) = resp.json::<Value>() {
                    if let Some(items) = v.get("items").and_then(|x| x.as_array()) {
                        for item in items {
                            if let Some(u) = item
                                .get("previewUrl")
                                .or_else(|| item.get("imageUrl"))
                                .and_then(|x| x.as_str())
                            {
                                if u.starts_with("http") && !meta.screenshot_urls.contains(&u.to_string())
                                {
                                    meta.screenshot_urls.push(u.to_string());
                                }
                            }
                            if meta.screenshot_urls.len() >= 3 {
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}

fn enrich_from_imdb(
    client: &Client,
    candidates: &[&str],
    year: Option<&str>,
    meta: &mut TorrentRichMetadata,
) {
    if meta.imdb_id.is_empty() {
        'outer: for q in candidates {
            let trimmed = q.trim();
            if trimmed.is_empty() {
                continue;
            }
            let encoded = urlencoding::encode(trimmed);
            let url = format!("https://v3.sg.media-imdb.com/suggestion/x/{encoded}.json");
            let Ok(resp) = client.get(&url).send() else {
                continue;
            };
            if !resp.status().is_success() {
                continue;
            }
            let Ok(v) = resp.json::<Value>() else {
                continue;
            };
            let Some(arr) = v.get("d").and_then(|x| x.as_array()) else {
                continue;
            };

            let title_items: Vec<&Value> = arr
                .iter()
                .filter(|item| {
                    item.get("id")
                        .and_then(|x| x.as_str())
                        .map(|s| s.starts_with("tt"))
                        .unwrap_or(false)
                })
                .collect();
            if title_items.is_empty() {
                continue;
            }

            let picked = year
                .and_then(|y| {
                    title_items.iter().copied().find(|item| {
                        item.get("y")
                            .and_then(|iy| iy.as_i64())
                            .map(|n| n.to_string() == y)
                            .unwrap_or(false)
                    })
                })
                .or_else(|| title_items.first().copied());

            if let Some(item) = picked {
                if let Some(id) = item.get("id").and_then(|x| x.as_str()) {
                    meta.imdb_id = id.to_string();
                }
                if meta.original_title.is_empty() {
                    if let Some(l) = item.get("l").and_then(|x| x.as_str()) {
                        meta.original_title = l.to_string();
                    }
                }
                if meta.year.is_empty() {
                    if let Some(y) = item.get("y").and_then(|x| x.as_i64()) {
                        meta.year = y.to_string();
                    }
                }
                if meta.poster_url.is_empty() {
                    if let Some(img) = item
                        .get("i")
                        .and_then(|i| i.get("imageUrl"))
                        .and_then(|x| x.as_str())
                    {
                        meta.poster_url = resize_amazon_image_url(img, 600);
                    }
                }
                break 'outer;
            }
        }
    }

    if meta.imdb_id.is_empty() {
        return;
    }

    // Fetch IMDb title page JSON-LD + media frames
    let title_url = format!("https://www.imdb.com/title/{}/", meta.imdb_id);
    if let Ok(html) = fetch_text_url(client, &title_url, None) {
        if let Some(ld) = extract_imdb_json_ld(&html) {
            if meta.rating_imdb.is_empty() {
                if let Some(rv) = ld
                    .get("aggregateRating")
                    .and_then(|a| a.get("ratingValue"))
                    .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
                {
                    if rv > 0.0 {
                        meta.rating_imdb = format!("{rv:.1}");
                    }
                }
            }
            if meta.genres.is_empty() {
                if let Some(arr) = ld.get("genre").and_then(|g| g.as_array()) {
                    let list: Vec<String> = arr
                        .iter()
                        .filter_map(|x| x.as_str())
                        .map(|s| s.to_string())
                        .take(4)
                        .collect();
                    meta.genres = list.join(", ");
                } else if let Some(s) = ld.get("genre").and_then(|g| g.as_str()) {
                    meta.genres = s.to_string();
                }
            }
            if meta.description.is_empty() {
                if let Some(desc) = ld.get("description").and_then(|x| x.as_str()) {
                    meta.description = decode_basic_entities(desc.trim());
                }
            }
            if meta.poster_url.is_empty() {
                if let Some(img) = ld.get("image").and_then(|x| x.as_str()) {
                    meta.poster_url = resize_amazon_image_url(img, 600);
                }
            }
            if meta.screenshot_urls.is_empty() {
                if let Some(thumb) = ld
                    .get("trailer")
                    .and_then(|t| t.get("thumbnailUrl"))
                    .and_then(|x| x.as_str())
                {
                    if thumb.starts_with("http") {
                        meta.screenshot_urls
                            .push(resize_amazon_image_url(thumb, 720));
                    }
                }
            }
        }

        // Collect additional stills from media-amazon URLs embedded in page if < 3 screenshots
        if meta.screenshot_urls.len() < 3 {
            let mut scan = 0;
            while let Some(rel) = html[scan..].find("https://m.media-amazon.com/images/M/") {
                let start = scan + rel;
                let end = html[start..]
                    .find(['"', '\'', ' ', '\\', '<'])
                    .map(|e| start + e)
                    .unwrap_or(start);
                let raw_u = &html[start..end];
                scan = end + 1;
                if raw_u.contains(".jpg") || raw_u.contains(".png") || raw_u.contains(".webp") {
                    let sized = resize_amazon_image_url(raw_u, 720);
                    if sized != meta.poster_url && !meta.screenshot_urls.contains(&sized) {
                        meta.screenshot_urls.push(sized);
                    }
                }
                if meta.screenshot_urls.len() >= 3 {
                    break;
                }
            }
        }

        if meta.source_label.is_empty() {
            meta.source_label = "IMDb".to_string();
        } else if !meta.source_label.contains("IMDb") {
            meta.source_label = format!("{} + IMDb", meta.source_label);
        }
    }
}

fn enrich_russian_summary_from_wikipedia(
    client: &Client,
    imdb_id: &str,
    clean_ru: &str,
    clean_en: Option<&str>,
    year: Option<&str>,
    meta: &mut TorrentRichMetadata,
) {
    // 1. Exact lookup via Wikidata P345 (IMDb ID) -> ruwiki page
    if !imdb_id.is_empty() {
        for sr_query in [format!("haswbstatement:P345={imdb_id}"), imdb_id.to_string()] {
            let wd_search = format!(
                "https://www.wikidata.org/w/api.php?action=query&list=search&srsearch={}&format=json&srlimit=1",
                urlencoding::encode(&sr_query)
            );
            if let Ok(resp) = client.get(&wd_search).send() {
                if let Ok(v) = resp.json::<Value>() {
                    if let Some(qid) = v
                        .get("query")
                        .and_then(|q| q.get("search"))
                        .and_then(|s| s.as_array())
                        .and_then(|a| a.first())
                        .and_then(|it| it.get("title"))
                        .and_then(|t| t.as_str())
                    {
                        let ent_url = format!(
                            "https://www.wikidata.org/w/api.php?action=wbgetentities&ids={qid}&props=sitelinks&sitefilter=ruwiki&format=json"
                        );
                        if let Ok(r2) = client.get(&ent_url).send() {
                            if let Ok(v2) = r2.json::<Value>() {
                                if let Some(ru_page) = v2
                                    .get("entities")
                                    .and_then(|e| e.get(qid))
                                    .and_then(|e| e.get("sitelinks"))
                                    .and_then(|s| s.get("ruwiki"))
                                    .and_then(|r| r.get("title"))
                                    .and_then(|t| t.as_str())
                                {
                                    if apply_ruwiki_page(client, ru_page, meta) {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // 2. Fallback: Russian Wikipedia search by cleaned Russian title, then English title
    let mut queries: Vec<String> = Vec::new();
    let ru_trimmed = clean_ru.trim();
    if !ru_trimmed.is_empty() && contains_cyrillic(ru_trimmed) {
        if let Some(y) = year.filter(|s| !s.is_empty()) {
            queries.push(format!("{ru_trimmed} {y}"));
        }
        queries.push(ru_trimmed.to_string());
    }
    if let Some(en) = clean_en.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(y) = year.filter(|s| !s.is_empty()) {
            queries.push(format!("{en} {y}"));
        }
        queries.push(en.to_string());
    }

    for q_str in queries {
        let wp_url = format!(
            "https://ru.wikipedia.org/w/api.php?action=query&list=search&srsearch={}&format=json&srlimit=3",
            urlencoding::encode(&q_str)
        );
        if let Ok(resp) = client.get(&wp_url).send() {
            if let Ok(v) = resp.json::<Value>() {
                if let Some(arr) = v
                    .get("query")
                    .and_then(|q| q.get("search"))
                    .and_then(|s| s.as_array())
                {
                    for item in arr {
                        if let Some(page_title) = item.get("title").and_then(|t| t.as_str()) {
                            let low_t = page_title.to_lowercase();
                            let snippet = item
                                .get("snippet")
                                .and_then(|s| s.as_str())
                                .unwrap_or("")
                                .to_lowercase();
                            let looks_media = low_t.contains("фильм")
                                || low_t.contains("сериал")
                                || snippet.contains("фильм")
                                || snippet.contains("сериал")
                                || snippet.contains("телесериал")
                                || year.map(|y| snippet.contains(y)).unwrap_or(false)
                                || arr.len() == 1;
                            if looks_media && apply_ruwiki_page(client, page_title, meta) {
                                return;
                            }
                        }
                    }
                    if let Some(first_title) = arr
                        .first()
                        .and_then(|it| it.get("title"))
                        .and_then(|t| t.as_str())
                    {
                        if apply_ruwiki_page(client, first_title, meta) {
                            return;
                        }
                    }
                }
            }
        }
    }
}

fn apply_ruwiki_page(client: &Client, page_title: &str, meta: &mut TorrentRichMetadata) -> bool {
    let Some((ru_title, summary, img_opt)) = fetch_ruwiki_summary(client, page_title) else {
        return false;
    };
    if meta.description.is_empty() || !contains_cyrillic(&meta.description) {
        meta.description = summary;
    }
    if !contains_cyrillic(&meta.title) && contains_cyrillic(&ru_title) {
        let stripped = ru_title
            .split(" (")
            .next()
            .unwrap_or(&ru_title)
            .trim();
        if !stripped.is_empty() {
            meta.title = stripped.to_string();
        }
    }
    if meta.poster_url.is_empty() {
        if let Some(img) = img_opt {
            if img.starts_with("http") {
                meta.poster_url = img;
            }
        }
    }
    if !meta.source_label.contains("Wikipedia") {
        meta.source_label = if meta.source_label.is_empty() {
            "Wikipedia".to_string()
        } else {
            format!("{} + Wikipedia", meta.source_label)
        };
    }
    true
}

fn fetch_ruwiki_summary(
    client: &Client,
    page_title: &str,
) -> Option<(String, String, Option<String>)> {
    let url = format!(
        "https://ru.wikipedia.org/api/rest_v1/page/summary/{}",
        urlencoding::encode(page_title.replace(' ', "_").as_str())
    );
    let resp = client.get(&url).send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: Value = resp.json().ok()?;
    let extract = v.get("extract").and_then(|x| x.as_str())?.trim();
    if extract.len() < 30 {
        return None;
    }
    let title = v
        .get("title")
        .and_then(|x| x.as_str())
        .unwrap_or(page_title)
        .trim()
        .to_string();
    let img = v
        .get("thumbnail")
        .and_then(|t| t.get("source"))
        .or_else(|| v.get("originalimage").and_then(|t| t.get("source")))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    Some((title, extract.to_string(), img))
}

fn resize_amazon_image_url(url: &str, width: u32) -> String {
    if let Some(idx) = url.find("._V1_") {
        if let Some(ext_dot) = url[idx..].rfind('.') {
            let prefix = &url[..idx];
            let ext = &url[idx + ext_dot..];
            return format!("{prefix}._V1_UX{width}{ext}");
        }
    }
    url.to_string()
}

fn extract_imdb_json_ld(html: &str) -> Option<Value> {
    let marker = "type=\"application/ld+json\"";
    let pos = html.find(marker)?;
    let gt = html[pos..].find('>')?;
    let tail = &html[pos + gt + 1..];
    let end = tail.find("</script>")?;
    serde_json::from_str::<Value>(tail[..end].trim()).ok()
}

pub fn extract_clean_title_and_year(raw: &str) -> (String, Option<String>, Option<String>) {
    let mut year: Option<String> = None;
    for tok in raw.split(|c: char| !c.is_ascii_digit()) {
        if tok.len() == 4 {
            if let Ok(y) = tok.parse::<u32>() {
                if (1950..=2035).contains(&y) {
                    year = Some(tok.to_string());
                    break;
                }
            }
        }
    }

    // Strip `[...]` and `(...)` blocks
    let mut no_brackets = String::with_capacity(raw.len());
    let mut depth_sq = 0i32;
    let mut depth_par = 0i32;
    for ch in raw.chars() {
        match ch {
            '[' => depth_sq += 1,
            ']' => depth_sq = (depth_sq - 1).max(0),
            '(' => depth_par += 1,
            ')' => depth_par = (depth_par - 1).max(0),
            _ if depth_sq == 0 && depth_par == 0 => no_brackets.push(ch),
            _ => {}
        }
    }

    let parts: Vec<String> = no_brackets
        .split(" / ")
        .map(|p| clean_release_segment(p))
        .filter(|p| !p.is_empty())
        .collect();

    let mut ru_part = String::new();
    let mut en_part: Option<String> = None;

    for p in &parts {
        if contains_cyrillic(p) && ru_part.is_empty() {
            ru_part = p.clone();
        } else if !contains_cyrillic(p) && en_part.is_none() {
            en_part = Some(p.clone());
        }
    }
    if ru_part.is_empty() {
        ru_part = parts.first().cloned().unwrap_or_default();
    }

    (ru_part, en_part, year)
}

fn clean_release_segment(seg: &str) -> String {
    let stop_words = [
        "1080p", "2160p", "720p", "480p", "4k", "uhd", "fhd", "bdrip", "bdremux", "bluray",
        "blu-ray", "web-dl", "webdl", "web-dlrip", "webrip", "hdrip", "hdtv", "dvdrip", "hdr",
        "hdr10", "dovi", "hevc", "h264", "h265", "x264", "x265", "10bit", "8bit", "ac3", "dts",
        "aac", "rus", "eng", "sub", "dub", "lostfilm", "hdrezka", "jaskier", "red", "season",
        "сезон", "серии", "дубляж", "лицензия", "itunes", "remux", "truehd",
    ];
    let normalized = split_glued_release_tags(&seg.replace('.', " ").replace('_', " "));
    let mut kept = Vec::new();
    for word in normalized.split_whitespace() {
        let low = word.to_lowercase();
        let clean_low = low.trim_matches(|c: char| !c.is_alphanumeric());
        if stop_words.contains(&clean_low) {
            break;
        }
        // Stop on season/episode tags like S01, S01E02
        if low.starts_with('s')
            && low.len() >= 3
            && low[1..3].chars().all(|c| c.is_ascii_digit())
        {
            break;
        }
        // Stop on standalone release year if it comes after title words
        if !kept.is_empty()
            && clean_low.len() == 4
            && clean_low
                .parse::<u32>()
                .map(|y| (1950..=2035).contains(&y))
                .unwrap_or(false)
        {
            break;
        }
        kept.push(word);
    }
    kept.join(" ")
        .trim_matches(|c: char| c == '-' || c == '|' || c == ',' || c.is_whitespace())
        .to_string()
}

fn split_glued_release_tags(input: &str) -> String {
    let mut s = input.to_string();
    // Separate glued S01..S99 season tags (e.g. GangsterlendS01WEBDL2160p -> Gangsterlend S01WEBDL2160p)
    let split_pos = {
        let bytes = s.as_bytes();
        (1..bytes.len().saturating_sub(2)).find(|&i| {
            (bytes[i] == b'S' || bytes[i] == b's')
                && bytes[i - 1].is_ascii_alphabetic()
                && bytes[i + 1].is_ascii_digit()
                && bytes[i + 2].is_ascii_digit()
        })
    };
    if let Some(pos) = split_pos {
        s.insert(pos, ' ');
    }
    for marker in [
        "2160p", "1080p", "720p", "480p", "WEBDL", "WEB-DL", "WEBRip", "BluRay", "BDRip",
        "BDRemux", "REMUX", "HEVC", "HDTV", "HDRip", "DVDRip",
    ] {
        if let Some(idx) = s.find(marker) {
            if idx > 0 && !s[..idx].ends_with(' ') {
                s.insert(idx, ' ');
            }
        }
    }
    s
}

fn split_camel_case(input: &str) -> String {
    if input.contains(' ') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len() + 4);
    let mut prev_lower = false;
    for ch in input.chars() {
        if prev_lower && ch.is_ascii_uppercase() {
            out.push(' ');
        }
        out.push(ch);
        prev_lower = ch.is_ascii_lowercase();
    }
    out
}

pub fn extract_rutracker_topic_id_from_filename(raw: &str) -> Option<String> {
    let low = raw.to_lowercase();
    let idx = low.find("rutracker-")?;
    let rest = &low[idx + "rutracker-".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 4 {
        Some(digits)
    } else {
        None
    }
}

fn contains_cyrillic(s: &str) -> bool {
    s.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c))
}

fn extract_rutracker_topic_id(url: &str) -> Option<String> {
    let pos = url.find("viewtopic.php?t=").or_else(|| url.find("t="))?;
    let marker = if url[pos..].starts_with("viewtopic.php?t=") {
        "viewtopic.php?t="
    } else {
        "t="
    };
    let rest = &url[pos + marker.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 4 {
        Some(digits)
    } else {
        None
    }
}

fn extract_kp_id_from_text(s: &str) -> Option<String> {
    for pat in ["kinopoisk.ru/film/", "kinopoisk.ru/series/"] {
        if let Some(pos) = s.find(pat) {
            let rest = &s[pos + pat.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.len() >= 3 {
                return Some(digits);
            }
        }
    }
    None
}

fn extract_imdb_id_from_text(s: &str) -> Option<String> {
    let pat = "imdb.com/title/";
    let pos = s.find(pat)?;
    let rest = &s[pos + pat.len()..];
    if rest.starts_with("tt") {
        let digits: String = rest[2..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.len() >= 5 {
            return Some(format!("tt{digits}"));
        }
    }
    None
}

fn extract_images_from_spoiler_blocks(scope: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut scan = 0;
    while let Some(rel) = scope[scan..].find("class=\"sp-body\"") {
        let start = scan + rel;
        let end = scope[start..]
            .find("</div>")
            .map(|e| start + e)
            .unwrap_or_else(|| (start + 4096).min(scope.len()));
        let block = &scope[start..end];
        let mut s2 = 0;
        while let Some(r2) = block[s2..].find("class=\"postImg") {
            let p2 = s2 + r2;
            let t_end = block[p2..].find('>').map(|e| p2 + e).unwrap_or(p2);
            let attrs = &block[p2..t_end];
            if let Some(url) = extract_attr_value(attrs, "title=\"")
                .or_else(|| extract_attr_value(attrs, "src=\""))
            {
                if !is_junk_tracker_image(&url) && !out.contains(&url) {
                    out.push(url);
                }
            }
            s2 = t_end + 1;
            if out.len() >= 4 {
                return out;
            }
        }
        scan = end + 1;
    }
    out
}

fn is_junk_tracker_image(url: &str) -> bool {
    let low = url.to_lowercase();
    low.contains("imdb.com")
        || low.contains("kinopoisk.ru/rating")
        || low.contains("static.rutracker")
        || low.contains("/smilies/")
        || low.contains("/smiles/")
        || low.contains("counter")
        || low.ends_with(".gif")
        || low.contains("magnet")
        || low.contains("dl.php")
}

fn extract_attr_value(attrs: &str, marker: &str) -> Option<String> {
    let pos = attrs.find(marker)?;
    let quote = if marker.ends_with('\'') { '\'' } else { '"' };
    let rest = &attrs[pos + marker.len()..];
    let end = rest.find(quote)?;
    let val = rest[..end].trim();
    if val.is_empty() {
        None
    } else {
        Some(val.to_string())
    }
}

fn url_origin(url: &str) -> Option<String> {
    let scheme_end = url.find("://")?;
    let after_scheme = &url[scheme_end + 3..];
    let host_end = after_scheme.find('/').unwrap_or(after_scheme.len());
    Some(url[..scheme_end + 3 + host_end].to_string())
}

fn html_to_lines(raw: &str) -> Vec<String> {
    let normalized = raw
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("</div>", "\n")
        .replace("</p>", "\n")
        .replace("</tr>", "\n");
    let mut no_tags = String::with_capacity(normalized.len());
    let mut in_tag = false;
    for ch in normalized.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => no_tags.push(ch),
            _ => {}
        }
    }
    decode_basic_entities(&no_tags)
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.trim().is_empty())
        .collect()
}

fn decode_basic_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace('\u{00a0}', " ")
}
