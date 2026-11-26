use crate::core::diagnostics;
use crate::core::storage::TorrServerConfig;
use crate::modules::torrserver::api::{
    detect_quality, format_bytes, parse_size_bytes, ApiSearchResult,
};
use encoding_rs::WINDOWS_1251;
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, ACCEPT_LANGUAGE};
use reqwest::Proxy;
use std::error::Error as StdError;
use std::sync::Mutex;
use std::time::Duration;

const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

pub enum RuTrackerPayload {
    TorrentBytes { bytes: Vec<u8>, filename: String },
    Magnet(String),
}

struct CachedSession {
    config_key: String,
    client: Client,
    logged_in: bool,
}

pub struct RuTrackerClient {
    session: Mutex<Option<CachedSession>>,
}

impl Default for RuTrackerClient {
    fn default() -> Self {
        Self::new()
    }
}

impl RuTrackerClient {
    pub fn new() -> Self {
        Self {
            session: Mutex::new(None),
        }
    }

    pub fn invalidate(&self) {
        if let Ok(mut guard) = self.session.lock() {
            *guard = None;
        }
    }

    pub fn verify_login(&self, cfg: &TorrServerConfig) -> Result<String, String> {
        self.invalidate();
        if !cfg.has_rutracker_credentials() {
            return Err("Укажите логин и пароль RuTracker (или cookie bb_session)".to_string());
        }
        let client = build_http_client(cfg)?;
        self.authenticate(&client, cfg)?;
        let base = cfg.effective_rutracker_mirror();
        let html = self.get_text_cp1251(&client, cfg, &format!("{base}/forum/tracker.php"))?;
        if looks_like_login_page(&html) {
            return Err(
                "Не удалось подтвердить сессию RuTracker (сервер вернул форму входа)".to_string(),
            );
        }
        let key = session_key(cfg);
        if let Ok(mut guard) = self.session.lock() {
            *guard = Some(CachedSession {
                config_key: key,
                client,
                logged_in: true,
            });
        }
        diagnostics::info("[TS:RUTRACKER]", "Авторизация на RuTracker успешно проверена");
        Ok(format!("Вход на RuTracker выполнен ({base})"))
    }

    pub fn search(&self, cfg: &TorrServerConfig, query: &str) -> Result<Vec<ApiSearchResult>, String> {
        if !cfg.has_rutracker_credentials() {
            return Err(
                "RuTracker не настроен: укажите логин и пароль в Настройках".to_string(),
            );
        }

        let base = cfg.effective_rutracker_mirror();
        let encoded_q = encode_cp1251_query(query.trim());
        let search_url = format!("{base}/forum/tracker.php?nm={encoded_q}&o=10&s=2");

        let client = self.ensure_client(cfg, false)?;
        let mut html = self.get_text_cp1251(&client, cfg, &search_url)?;

        if looks_like_login_page(&html) && cfg.rutracker_cookie.trim().is_empty() {
            diagnostics::info("[TS:RUTRACKER]", "Сессия истекла — повторный вход на RuTracker");
            let fresh_client = self.ensure_client(cfg, true)?;
            html = self.get_text_cp1251(&fresh_client, cfg, &search_url)?;
        }

        if looks_like_login_page(&html) {
            return Err(
                "RuTracker требует авторизацию (проверьте логин/пароль или cookie bb_session)"
                    .to_string(),
            );
        }

        let results = parse_tracker_html(&html);
        diagnostics::info(
            "[TS:RUTRACKER]",
            format!("Поиск RuTracker по «{}» вернул {} результатов", query.trim(), results.len()),
        );
        Ok(results)
    }

    pub fn fetch_torrent_or_magnet(
        &self,
        cfg: &TorrServerConfig,
        topic_id: &str,
    ) -> Result<RuTrackerPayload, String> {
        let base = cfg.effective_rutracker_mirror();
        let dl_url = format!("{base}/forum/dl.php?t={topic_id}");
        let topic_url = format!("{base}/forum/viewtopic.php?t={topic_id}");

        let client = self.ensure_client(cfg, false)?;
        let mut bytes = self.get_bytes(&client, cfg, &dl_url, Some(&topic_url))?;

        if !bytes.starts_with(b"d") {
            let html_check = decode_cp1251(&bytes);
            if looks_like_login_page(&html_check) && cfg.rutracker_cookie.trim().is_empty() {
                let fresh = self.ensure_client(cfg, true)?;
                bytes = self.get_bytes(&fresh, cfg, &dl_url, Some(&topic_url))?;
            }
        }

        if bytes.starts_with(b"d") && bytes.len() > 32 {
            return Ok(RuTrackerPayload::TorrentBytes {
                bytes,
                filename: format!("rutracker_{topic_id}.torrent"),
            });
        }

        // Fallback: parse magnet link directly from viewtopic page
        let client2 = self.ensure_client(cfg, false)?;
        let topic_html = self.get_text_cp1251(&client2, cfg, &topic_url)?;
        if let Some(magnet) = extract_magnet_from_html(&topic_html) {
            return Ok(RuTrackerPayload::Magnet(magnet));
        }

        Err(format!(
            "Не удалось скачать .torrent или получить magnet с темы RuTracker #{topic_id}"
        ))
    }

    fn ensure_client(&self, cfg: &TorrServerConfig, force_refresh: bool) -> Result<Client, String> {
        let key = session_key(cfg);
        if !force_refresh {
            if let Ok(guard) = self.session.lock() {
                if let Some(cached) = guard.as_ref() {
                    if cached.config_key == key && cached.logged_in {
                        return Ok(cached.client.clone());
                    }
                }
            }
        }

        let client = build_http_client(cfg)?;
        self.authenticate(&client, cfg)?;
        if let Ok(mut guard) = self.session.lock() {
            *guard = Some(CachedSession {
                config_key: key,
                client: client.clone(),
                logged_in: true,
            });
        }
        Ok(client)
    }

    fn authenticate(&self, client: &Client, cfg: &TorrServerConfig) -> Result<(), String> {
        if !cfg.rutracker_cookie.trim().is_empty() {
            return Ok(());
        }

        let user = cfg.rutracker_username.trim();
        let pass = cfg.rutracker_password.as_str();
        if user.is_empty() || pass.is_empty() {
            return Err("Не указаны логин или пароль RuTracker".to_string());
        }

        let base = cfg.effective_rutracker_mirror();
        let login_url = format!("{base}/forum/login.php");
        let body = format!(
            "login_username={}&login_password={}&login=%C2%F5%EE%E4",
            encode_cp1251_query(user),
            encode_cp1251_query(pass)
        );

        let resp = client
            .post(&login_url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Referer", format!("{base}/forum/index.php"))
            .body(body)
            .send()
            .map_err(|e| {
                format!(
                    "Ошибка подключения к RuTracker ({base}): {}",
                    format_error_chain(&e)
                )
            })?;

        let status = resp.status();
        let headers = resp.headers().clone();
        let raw_bytes = resp
            .bytes()
            .map_err(|e| format!("Ошибка чтения ответа авторизации RuTracker: {e}"))?;
        let html = decode_cp1251(&raw_bytes);
        if !status.is_success() {
            return Err(diagnose_http_error(status.as_u16(), &headers, &html, "входе"));
        }
        let low = html.to_lowercase();
        if low.contains("cap_sid") || low.contains("cap_code") || low.contains("код подтверждения") {
            return Err(
                "RuTracker запросил капчу при входе. Вставьте cookie bb_session из браузера в Настройках или попробуйте позже."
                    .to_string(),
            );
        }
        if low.contains("неверное/неактивное имя") || low.contains("неверный пароль") {
            return Err("Неверный логин или пароль RuTracker".to_string());
        }

        Ok(())
    }

    fn get_text_cp1251(
        &self,
        client: &Client,
        cfg: &TorrServerConfig,
        url: &str,
    ) -> Result<String, String> {
        let bytes = self.get_bytes(client, cfg, url, None)?;
        Ok(decode_cp1251(&bytes))
    }

    fn get_bytes(
        &self,
        client: &Client,
        cfg: &TorrServerConfig,
        url: &str,
        referer: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        let mut req = client.get(url);
        if let Some(ref_url) = referer {
            req = req.header("Referer", ref_url);
        }
        if let Some(cookie_hdr) = normalized_cookie_header(&cfg.rutracker_cookie) {
            req = req.header("Cookie", cookie_hdr);
        }
        let resp = req.send().map_err(|e| {
            format!(
                "Ошибка запроса к RuTracker: {}",
                format_error_chain(&e)
            )
        })?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let raw_bytes = resp
            .bytes()
            .map_err(|e| format!("Ошибка чтения ответа RuTracker: {e}"))?;
        if !status.is_success() {
            let preview = decode_cp1251(&raw_bytes[..raw_bytes.len().min(4096)]);
            return Err(diagnose_http_error(status.as_u16(), &headers, &preview, "запросе"));
        }
        Ok(raw_bytes.to_vec())
    }
}

fn diagnose_http_error(code: u16, headers: &HeaderMap, body_preview: &str, stage: &str) -> String {
    let low = body_preview.to_lowercase();
    let is_cf = headers.contains_key("cf-mitigated")
        || headers.contains_key("cf-ray")
        || low.contains("cloudflare")
        || low.contains("just a moment")
        || low.contains("attention required")
        || low.contains("cf-browser-verification");

    if code == 403 && is_cf {
        return format!(
            "RuTracker HTTP 403 (Cloudflare блокирует автоматический запрос при {stage}). Укажите другое зеркало (например rutracker.net) или вставьте полную строку Cookie (bb_session=...; cf_clearance=...) из браузера."
        );
    }
    if code == 403 {
        return format!(
            "RuTracker HTTP 403 при {stage} (доступ запрещён для текущего IP/зеркала или устаревшая cookie)"
        );
    }
    format!("RuTracker HTTP {code}")
}

fn format_error_chain(err: &dyn StdError) -> String {
    let mut parts = vec![err.to_string()];
    let mut cur = err.source();
    while let Some(src) = cur {
        let s = src.to_string();
        if !parts.last().map(|p| p == &s).unwrap_or(false) {
            parts.push(s);
        }
        cur = src.source();
    }
    parts.join(" -> ")
}

fn session_key(cfg: &TorrServerConfig) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        cfg.effective_rutracker_mirror(),
        cfg.rutracker_username.trim(),
        cfg.rutracker_password,
        cfg.rutracker_proxy.trim(),
        cfg.rutracker_cookie.trim()
    )
}

fn normalized_cookie_header(raw: &str) -> Option<String> {
    let mut cleaned = raw.replace(['\r', '\n'], " ").trim().to_string();
    if let Some(rest) = cleaned
        .strip_prefix("Cookie:")
        .or_else(|| cleaned.strip_prefix("cookie:"))
    {
        cleaned = rest.trim().to_string();
    }
    if cleaned.is_empty() {
        return None;
    }
    if cleaned.contains('=') {
        Some(cleaned)
    } else {
        Some(format!("bb_session={cleaned}"))
    }
}

fn build_http_client(cfg: &TorrServerConfig) -> Result<Client, String> {
    let mut default_headers = HeaderMap::new();
    default_headers.insert(
        ACCEPT,
        HeaderValue::from_static(
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8",
        ),
    );
    default_headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("ru-RU,ru;q=0.9,en-US;q=0.8,en;q=0.7"),
    );
    default_headers.insert(
        HeaderName::from_static("upgrade-insecure-requests"),
        HeaderValue::from_static("1"),
    );

    let mut builder = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .cookie_store(true)
        .default_headers(default_headers)
        .user_agent(USER_AGENT);

    let proxy_str = cfg.rutracker_proxy.trim();
    if !proxy_str.is_empty() {
        let proxy = Proxy::all(proxy_str)
            .map_err(|e| format!("Некорректный адрес прокси '{proxy_str}': {e}"))?;
        builder = builder.proxy(proxy);
    }

    builder
        .build()
        .map_err(|e| format!("Ошибка создания HTTP-клиента RuTracker: {e}"))
}

fn decode_cp1251(bytes: &[u8]) -> String {
    let (cow, _, _) = WINDOWS_1251.decode(bytes);
    cow.into_owned()
}

fn encode_cp1251_query(input: &str) -> String {
    let (cow, _, _) = WINDOWS_1251.encode(input);
    let mut out = String::with_capacity(cow.len() * 3);
    for &b in cow.as_ref() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

fn looks_like_login_page(html: &str) -> bool {
    let low = html.to_lowercase();
    low.contains("name=\"login_username\"")
        || low.contains("name='login_username'")
        || (low.contains("login.php") && low.contains("login_password"))
}

fn parse_tracker_html(html: &str) -> Vec<ApiSearchResult> {
    let mut results = Vec::new();

    // Narrow down to tor-tbl if present
    let scope = if let Some(idx) = html.find("id=\"tor-tbl\"") {
        let tail = &html[idx..];
        if let Some(end) = tail.find("</table>") {
            &tail[..end]
        } else {
            tail
        }
    } else {
        html
    };

    for row in scope.split("<tr") {
        let Some(row_end) = row.find("</tr>") else {
            continue;
        };
        let row_html = &row[..row_end];
        if !row_html.contains("viewtopic.php?t=") {
            continue;
        }

        let topic_id = extract_attr_digits(row_html, "data-topic_id=\"")
            .or_else(|| extract_attr_digits(row_html, "viewtopic.php?t="));
        let Some(tid) = topic_id else {
            continue;
        };

        let title = extract_topic_title(row_html, &tid);
        if title.is_empty() {
            continue;
        }

        let (size_text, size_bytes) = extract_size(row_html);
        let seeds = extract_class_int(row_html, "seedmed")
            .or_else(|| extract_class_int(row_html, "seed"))
            .unwrap_or(0);
        let peers = extract_class_int(row_html, "leechmed")
            .or_else(|| extract_class_int(row_html, "leech"))
            .unwrap_or(0);

        let quality = detect_quality(&title);
        results.push(ApiSearchResult {
            title,
            magnet: format!("rutracker://topic/{tid}"),
            size_text,
            size_bytes,
            seeds,
            peers,
            quality,
            tracker: "RuTracker".to_string(),
        });

        if results.len() >= 80 {
            break;
        }
    }

    results
}

fn extract_attr_digits(haystack: &str, marker: &str) -> Option<String> {
    let pos = haystack.find(marker)?;
    let rest = &haystack[pos + marker.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

fn extract_topic_title(row_html: &str, topic_id: &str) -> String {
    let needle = format!("viewtopic.php?t={topic_id}");
    let mut search_from = 0;
    while let Some(a_rel) = row_html[search_from..].find("<a ") {
        let a_start = search_from + a_rel;
        let Some(gt_rel) = row_html[a_start..].find('>') else {
            break;
        };
        let tag_end = a_start + gt_rel;
        let attrs = &row_html[a_start..tag_end];
        let Some(close_rel) = row_html[tag_end + 1..].find("</a>") else {
            break;
        };
        let inner = &row_html[tag_end + 1..tag_end + 1 + close_rel];
        search_from = tag_end + 1 + close_rel + 4;

        if attrs.contains(&needle) {
            let cleaned = clean_html_text(inner);
            if !cleaned.is_empty() {
                return cleaned;
            }
        }
    }
    String::new()
}

fn extract_size(row_html: &str) -> (String, f64) {
    if let Some(pos) = row_html.find("tor-size") {
        let tail = &row_html[pos..];
        let td_end = tail.find("</td>").unwrap_or(tail.len());
        let cell = &tail[..td_end];

        if let Some(bytes_str) = extract_attr_digits(cell, "data-ts_text=\"") {
            if let Ok(bytes) = bytes_str.parse::<u64>() {
                if bytes > 0 {
                    return (format_bytes(bytes), bytes as f64);
                }
            }
        }

        if let Some(gt) = cell.find('>') {
            let text = clean_html_text(&cell[gt + 1..]);
            if !text.is_empty() {
                let bytes = parse_size_bytes(&text);
                return (text, bytes);
            }
        }
    }
    ("—".to_string(), 0.0)
}

fn extract_class_int(row_html: &str, class_name: &str) -> Option<i32> {
    let pos = row_html.find(class_name)?;
    let tail = &row_html[pos..];
    let gt = tail.find('>')?;
    let after = &tail[gt + 1..];
    let end = after
        .find("</td>")
        .or_else(|| after.find("</b>"))
        .unwrap_or_else(|| after.len().min(64));
    let txt = clean_html_text(&after[..end]);
    let digits: String = txt
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<i32>().ok()
}

fn extract_magnet_from_html(html: &str) -> Option<String> {
    let pos = html.find("magnet:?xt=")?;
    let rest = &html[pos..];
    let end = rest
        .find(['"', '\'', '<', '>', ' ', '\n', '\r'])
        .unwrap_or(rest.len());
    let raw = &rest[..end];
    let decoded = decode_html_entities(raw);
    if decoded.starts_with("magnet:?xt=") {
        Some(decoded)
    } else {
        None
    }
}

fn clean_html_text(raw: &str) -> String {
    let mut no_tags = String::with_capacity(raw.len());
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => no_tags.push(ch),
            _ => {}
        }
    }
    let decoded = decode_html_entities(&no_tags);
    decoded
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn decode_html_entities(s: &str) -> String {
    let replaced = s
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("\u{00a0}", " ");

    // Decode numeric entities like &#1234; or &#x1F;
    let mut out = String::with_capacity(replaced.len());
    let mut rest = replaced.as_str();
    while let Some(idx) = rest.find("&#") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 2..];
        if let Some(semi) = after.find(';') {
            let code_str = &after[..semi];
            if code_str.len() <= 8 {
                let parsed = if let Some(hex) = code_str.strip_prefix('x').or_else(|| code_str.strip_prefix('X')) {
                    u32::from_str_radix(hex, 16).ok()
                } else {
                    code_str.parse::<u32>().ok()
                };
                if let Some(ch) = parsed.and_then(char::from_u32) {
                    out.push(ch);
                    rest = &after[semi + 1..];
                    continue;
                }
            }
        }
        out.push_str("&#");
        rest = after;
    }
    out.push_str(rest);
    out
}
