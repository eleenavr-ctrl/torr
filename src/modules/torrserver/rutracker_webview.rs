use crate::core::diagnostics;
use crate::core::storage::TorrServerConfig;
use crate::modules::torrserver::api::ApiSearchResult;
use crate::modules::torrserver::rutracker::{
    extract_magnet_from_html, looks_like_login_page, parse_tracker_html, RuTrackerPayload,
};
use std::path::PathBuf;

#[cfg(windows)]
mod win_impl {
    use super::*;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
    use serde::Deserialize;
    use std::cell::RefCell;
    use std::collections::{HashMap, VecDeque};
    use std::fs;
    use std::num::NonZeroIsize;
    use std::rc::Rc;
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{COLOR_WINDOW, HBRUSH};
    use windows_sys::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, KillTimer,
        LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer,
        SetWindowTextW, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
        IDC_ARROW, MSG, SW_HIDE, SW_RESTORE, SW_SHOW, WM_APP, WM_CLOSE, WM_TIMER, WNDCLASSW,
        WS_OVERLAPPEDWINDOW,
    };
    use wry::{PageLoadEvent, ProxyConfig, ProxyEndpoint, WebContext, WebView, WebViewBuilder};

    const WM_WV_CMD: u32 = WM_APP + 21;
    const WM_WV_PAGE_FINISHED: u32 = WM_APP + 22;
    const WM_WV_QUIT: u32 = WM_APP + 23;
    const TIMER_NAV_ID: usize = 1001;

    const PAGE_PROBE_JS: &str = r#"(function(){
        try {
            var title = document.title || '';
            var lowTitle = title.toLowerCase();
            var hasCf = (
                lowTitle.indexOf('just a moment') !== -1 ||
                lowTitle.indexOf('attention required') !== -1 ||
                lowTitle.indexOf('один момент') !== -1 ||
                !!document.querySelector('#challenge-form, #challenge-running, .cf-turnstile, #cf-please-wait')
            );
            var loggedIn = !!(
                document.querySelector('a[href*="login.php?logout=1"]') ||
                document.querySelector('#logged-in-username') ||
                document.querySelector('a[href*="profile.php?mode=viewprofile"]')
            );
            var hasLoginForm = !!(
                document.querySelector('input[name="login_username"]') &&
                document.querySelector('input[name="login_password"]')
            );
            var hasResults = !!document.querySelector('#tor-tbl');
            var userEl = document.querySelector('#logged-in-username');
            var user = userEl ? (userEl.textContent || '').trim() : '';
            var href = String(location.href || '');
            var html = (!hasCf && (hasResults || href.indexOf('tracker.php') !== -1))
                ? (document.documentElement ? document.documentElement.outerHTML : '')
                : '';
            window.ipc.postMessage(JSON.stringify({
                kind: 'page_ready',
                url: href,
                hasCf: hasCf,
                loggedIn: loggedIn,
                hasLoginForm: hasLoginForm,
                hasResults: hasResults,
                user: user,
                html: html
            }));
        } catch (e) {
            window.ipc.postMessage(JSON.stringify({
                kind: 'page_ready',
                url: '',
                hasCf: false,
                loggedIn: false,
                hasLoginForm: false,
                hasResults: false,
                user: '',
                html: ''
            }));
        }
    })();"#;

    #[derive(Debug, Deserialize)]
    #[serde(tag = "kind")]
    enum WvIpcMessage {
        #[serde(rename = "page_ready")]
        PageReady {
            url: String,
            #[serde(rename = "hasCf")]
            has_cf: bool,
            #[serde(rename = "loggedIn")]
            logged_in: bool,
            #[serde(rename = "hasLoginForm")]
            has_login_form: bool,
            #[serde(rename = "hasResults")]
            has_results: bool,
            user: String,
            html: String,
        },
        #[serde(rename = "fetch_html")]
        FetchHtml {
            id: u64,
            ok: bool,
            status: u16,
            html: String,
        },
        #[serde(rename = "torrent")]
        Torrent { id: u64, b64: String },
        #[serde(rename = "topic_html")]
        TopicHtml {
            id: u64,
            ok: bool,
            status: u16,
            html: String,
        },
        #[serde(rename = "fetch_err")]
        FetchErr { id: u64, error: String },
    }

    #[derive(Debug, Clone)]
    struct PageReadyData {
        _url: String,
        logged_in: bool,
        has_login_form: bool,
        _has_results: bool,
        user: String,
        html: String,
    }

    enum WvFetchOutcome {
        Html { ok: bool, status: u16, html: String },
        TorrentBytes(Vec<u8>),
        TopicHtml { ok: bool, status: u16, html: String },
        Error(String),
    }

    struct PendingNav {
        reply: mpsc::Sender<Result<PageReadyData, String>>,
        deadline: Instant,
        saw_cf: bool,
    }

    #[derive(Clone)]
    struct WvInitConfig {
        user_agent: String,
        proxy: String,
    }

    impl WvInitConfig {
        fn signature(&self) -> String {
            format!("{}|{}", self.proxy.trim(), self.user_agent.trim())
        }
    }

    enum WvCommand {
        OpenLoginWindow {
            url: String,
            init_cfg: WvInitConfig,
            on_status: Option<Arc<dyn Fn(String) + Send + Sync>>,
            reply: mpsc::Sender<Result<(), String>>,
        },
        ShowWindow,
        NavigateAndWait {
            url: String,
            init_cfg: WvInitConfig,
            timeout_ms: u64,
            reply: mpsc::Sender<Result<PageReadyData, String>>,
        },
        RunFetch {
            init_cfg: WvInitConfig,
            base_url: String,
            js: String,
            req_id: u64,
            reply: mpsc::Sender<WvFetchOutcome>,
        },
        GetUrl {
            reply: mpsc::Sender<String>,
        },
    }

    struct WvThreadState {
        webview_dir: PathBuf,
        hwnd_raw: isize,
        hinstance_raw: isize,
        web_context: WebContext,
        webview: Option<Rc<WebView>>,
        config_sig: String,
        pending_fetches: Arc<Mutex<HashMap<u64, mpsc::Sender<WvFetchOutcome>>>>,
        pending_nav: Option<PendingNav>,
        interactive_login_open: bool,
        saw_login_or_cf_during_popup: bool,
        login_status_cb: Option<Arc<dyn Fn(String) + Send + Sync>>,
        cmd_queue: Arc<Mutex<VecDeque<WvCommand>>>,
    }

    thread_local! {
        static WV_STATE: RefCell<Option<WvThreadState>> = const { RefCell::new(None) };
    }

    struct RawWin32Window {
        hwnd: NonZeroIsize,
        hinstance: Option<NonZeroIsize>,
    }

    impl HasWindowHandle for RawWin32Window {
        fn window_handle(
            &self,
        ) -> Result<WindowHandle<'_>, raw_window_handle::HandleError> {
            let mut handle = Win32WindowHandle::new(self.hwnd);
            handle.hinstance = self.hinstance;
            Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(handle)) })
        }
    }

    struct WorkerClient {
        hwnd_raw: isize,
        cmd_queue: Arc<Mutex<VecDeque<WvCommand>>>,
        next_req_id: std::sync::atomic::AtomicU64,
        join_handle: Option<JoinHandle<()>>,
    }

    impl WorkerClient {
        fn send_cmd(&self, cmd: WvCommand) -> Result<(), String> {
            {
                let mut q = self
                    .cmd_queue
                    .lock()
                    .map_err(|_| "Очередь WebView2 заблокирована".to_string())?;
                q.push_back(cmd);
            }
            unsafe {
                let ok = PostMessageW(self.hwnd_raw as HWND, WM_WV_CMD, 0, 0);
                if ok == 0 {
                    return Err("Поток WebView2 завершён".to_string());
                }
            }
            Ok(())
        }

        fn next_id(&self) -> u64 {
            self.next_req_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    }

    pub struct RuTrackerWebView {
        webview_dir: PathBuf,
        worker: Mutex<Option<WorkerClient>>,
    }

    impl RuTrackerWebView {
        pub fn new(webview_dir: PathBuf) -> Self {
            Self {
                webview_dir,
                worker: Mutex::new(None),
            }
        }

        pub fn shutdown(&self) {
            if let Ok(mut guard) = self.worker.lock() {
                if let Some(mut client) = guard.take() {
                    unsafe {
                        PostMessageW(client.hwnd_raw as HWND, WM_WV_QUIT, 0, 0);
                    }
                    if let Some(h) = client.join_handle.take() {
                        let _ = h.join();
                    }
                }
            }
        }

        fn with_worker<F, T>(&self, f: F) -> Result<T, String>
        where
            F: FnOnce(&WorkerClient) -> Result<T, String>,
        {
            let mut guard = self
                .worker
                .lock()
                .map_err(|_| "Блокировка WebView2".to_string())?;

            let needs_spawn = match guard.as_ref() {
                None => true,
                Some(c) => c
                    .join_handle
                    .as_ref()
                    .map(|h| h.is_finished())
                    .unwrap_or(true),
            };

            if needs_spawn {
                let client = spawn_wv_worker(self.webview_dir.clone())?;
                *guard = Some(client);
            }

            let client = guard
                .as_ref()
                .ok_or_else(|| "Не удалось инициализировать WebView2".to_string())?;
            f(client)
        }

        pub fn open_login_window<F>(
            &self,
            cfg: &TorrServerConfig,
            on_status: F,
        ) -> Result<(), String>
        where
            F: Fn(String) + Send + Sync + 'static,
        {
            let base = cfg.effective_rutracker_mirror();
            let url = format!("{base}/forum/index.php");
            let init_cfg = make_init_cfg(cfg);
            let cb: Arc<dyn Fn(String) + Send + Sync> = Arc::new(on_status);

            self.with_worker(|client| {
                let (tx, rx) = mpsc::channel();
                client.send_cmd(WvCommand::OpenLoginWindow {
                    url,
                    init_cfg,
                    on_status: Some(cb),
                    reply: tx,
                })?;
                rx.recv_timeout(Duration::from_secs(15))
                    .map_err(|_| "Таймаут открытия окна WebView2".to_string())?
            })
        }

        pub fn verify_login(&self, cfg: &TorrServerConfig) -> Result<String, String> {
            let base = cfg.effective_rutracker_mirror();
            let tracker_url = format!("{base}/forum/tracker.php");
            let init_cfg = make_init_cfg(cfg);

            let page = self.navigate_and_wait(&tracker_url, &init_cfg, 18_000)?;
            if page.logged_in && !page.has_login_form {
                let who = if page.user.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", page.user)
                };
                diagnostics::info(
                    "[TS:RUTRACKER]",
                    format!("Сессия RuTracker подтверждена через WebView2{who}"),
                );
                return Ok(format!("Вход выполнен через браузер WebView2{who}"));
            }

            let _ = self.with_worker(|client| {
                let (tx, rx) = mpsc::channel();
                client.send_cmd(WvCommand::OpenLoginWindow {
                    url: format!("{base}/forum/login.php"),
                    init_cfg,
                    on_status: None,
                    reply: tx,
                })?;
                let _ = rx.recv_timeout(Duration::from_secs(10));
                Ok(())
            });

            Err("Вы не авторизованы в встроенном браузере: войдите в открывшемся окне RuTracker и нажмите «Сохранить и проверить вход» ещё раз.".to_string())
        }

        pub fn search_tracker_path(
            &self,
            cfg: &TorrServerConfig,
            rel_path: &str,
            log_label: &str,
        ) -> Result<Vec<ApiSearchResult>, String> {
            let base = cfg.effective_rutracker_mirror();
            let full_url = format!("{base}{rel_path}");
            let init_cfg = make_init_cfg(cfg);

            // Fast path: if WebView2 is already on the target mirror origin, run same-origin fetch()
            let cur_url = self.current_url().unwrap_or_default();
            if cur_url.starts_with(&base) {
                let req_id = self.next_id()?;
                let rel_json = serde_json::to_string(rel_path)
                    .map_err(|e| format!("Ошибка сериализации URL: {e}"))?;
                let js = format!(
                    r#"(async function(){{
                        try {{
                            var r = await fetch({rel_json}, {{ credentials: 'include' }});
                            var buf = await r.arrayBuffer();
                            var html = new TextDecoder('windows-1251').decode(buf);
                            window.ipc.postMessage(JSON.stringify({{ kind: 'fetch_html', id: {req_id}, ok: r.ok, status: r.status, html: html }}));
                        }} catch (e) {{
                            window.ipc.postMessage(JSON.stringify({{ kind: 'fetch_err', id: {req_id}, error: String(e) }}));
                        }}
                    }})();"#
                );

                if let Ok(WvFetchOutcome::Html { ok, status, html }) =
                    self.run_fetch(&init_cfg, &base, js, req_id, 20)
                {
                    if ok && !looks_like_cloudflare_html(&html) {
                        if looks_like_login_page(&html) {
                            let _ = self.show_window_at(&format!("{base}/forum/login.php"), &init_cfg);
                            return Err("Требуется вход в RuTracker: войдите в открывшемся окне браузера и повторите поиск.".to_string());
                        }
                        let results = parse_tracker_html(&html);
                        diagnostics::info(
                            "[TS:RUTRACKER]",
                            format!(
                                "Поиск RuTracker (WebView2) {log_label} вернул {} результатов",
                                results.len()
                            ),
                        );
                        return Ok(results);
                    }
                    diagnostics::info(
                        "[TS:RUTRACKER]",
                        format!("WebView2 fetch вернул HTTP {status} — переходим к загрузке страницы для прохождения Cloudflare"),
                    );
                }
            }

            // Full navigation path: automatically solves Cloudflare JS challenge inside WebView2
            let page = self.navigate_and_wait(&full_url, &init_cfg, 18_000)?;
            if page.has_login_form && !page.logged_in {
                let _ = self.with_worker(|c| c.send_cmd(WvCommand::ShowWindow));
                return Err("Требуется вход в RuTracker: войдите в открывшемся окне браузера и повторите поиск.".to_string());
            }

            let html = if !page.html.is_empty() {
                page.html
            } else {
                // Grab outerHTML explicitly if navigated URL redirected
                let req_id = self.next_id()?;
                let js = format!(
                    r#"(function(){{
                        try {{
                            var h = document.documentElement ? document.documentElement.outerHTML : '';
                            window.ipc.postMessage(JSON.stringify({{ kind: 'fetch_html', id: {req_id}, ok: true, status: 200, html: h }}));
                        }} catch (e) {{
                            window.ipc.postMessage(JSON.stringify({{ kind: 'fetch_err', id: {req_id}, error: String(e) }}));
                        }}
                    }})();"#
                );
                match self.run_fetch(&init_cfg, &base, js, req_id, 10)? {
                    WvFetchOutcome::Html { html, .. } => html,
                    WvFetchOutcome::Error(e) => return Err(e),
                    _ => String::new(),
                }
            };

            if looks_like_login_page(&html) {
                let _ = self.with_worker(|c| c.send_cmd(WvCommand::ShowWindow));
                return Err("Требуется вход в RuTracker: войдите в открывшемся окне браузера и повторите поиск.".to_string());
            }

            let results = parse_tracker_html(&html);
            diagnostics::info(
                "[TS:RUTRACKER]",
                format!(
                    "Поиск RuTracker (WebView2) {log_label} вернул {} результатов",
                    results.len()
                ),
            );
            Ok(results)
        }

        pub fn fetch_torrent_or_magnet(
            &self,
            cfg: &TorrServerConfig,
            topic_id: &str,
        ) -> Result<RuTrackerPayload, String> {
            let base = cfg.effective_rutracker_mirror();
            let init_cfg = make_init_cfg(cfg);
            let cur_url = self.current_url().unwrap_or_default();
            if !cur_url.starts_with(&base) {
                let _ = self.navigate_and_wait(
                    &format!("{base}/forum/viewtopic.php?t={topic_id}"),
                    &init_cfg,
                    15_000,
                )?;
            }

            let tid_json = serde_json::to_string(topic_id)
                .map_err(|e| format!("Ошибка сериализации topic_id: {e}"))?;
            let req_id = self.next_id()?;
            let js = format!(
                r#"(async function(){{
                    try {{
                        var tid = {tid_json};
                        var resp = await fetch('/forum/dl.php?t=' + tid, {{ credentials: 'include' }});
                        var buf = await resp.arrayBuffer();
                        var bytes = new Uint8Array(buf);
                        if (resp.ok && bytes.length > 32 && bytes[0] === 0x64) {{
                            var bin = '';
                            var chunk = 0x8000;
                            for (var i = 0; i < bytes.length; i += chunk) {{
                                bin += String.fromCharCode.apply(null, bytes.subarray(i, i + chunk));
                            }}
                            window.ipc.postMessage(JSON.stringify({{ kind: 'torrent', id: {req_id}, b64: btoa(bin) }}));
                            return;
                        }}
                        var tp = await fetch('/forum/viewtopic.php?t=' + tid, {{ credentials: 'include' }});
                        var tbuf = await tp.arrayBuffer();
                        var thtml = new TextDecoder('windows-1251').decode(tbuf);
                        window.ipc.postMessage(JSON.stringify({{ kind: 'topic_html', id: {req_id}, ok: tp.ok, status: tp.status, html: thtml }}));
                    }} catch (e) {{
                        window.ipc.postMessage(JSON.stringify({{ kind: 'fetch_err', id: {req_id}, error: String(e) }}));
                    }}
                }})();"#
            );

            match self.run_fetch(&init_cfg, &base, js, req_id, 25)? {
                WvFetchOutcome::TorrentBytes(bytes) => {
                    if bytes.starts_with(b"d") && bytes.len() > 32 {
                        return Ok(RuTrackerPayload::TorrentBytes {
                            bytes,
                            filename: format!("rutracker_{topic_id}.torrent"),
                        });
                    }
                }
                WvFetchOutcome::TopicHtml { ok, status, html } => {
                    if ok && !looks_like_cloudflare_html(&html) {
                        if let Some(magnet) = extract_magnet_from_html(&html) {
                            return Ok(RuTrackerPayload::Magnet(magnet));
                        }
                        if looks_like_login_page(&html) {
                            let _ = self.show_window_at(&format!("{base}/forum/login.php"), &init_cfg);
                            return Err("Требуется вход в RuTracker в окне WebView2".to_string());
                        }
                    } else if status == 403 {
                        // Retry via full page navigation so Cloudflare clears
                        let topic_url = format!("{base}/forum/viewtopic.php?t={topic_id}");
                        let page = self.navigate_and_wait(&topic_url, &init_cfg, 16_000)?;
                        if let Some(magnet) = extract_magnet_from_html(&page.html) {
                            return Ok(RuTrackerPayload::Magnet(magnet));
                        }
                    }
                }
                WvFetchOutcome::Error(e) => return Err(e),
                WvFetchOutcome::Html { .. } => {}
            }

            // Last-resort magnet fallback via page navigation
            let topic_url = format!("{base}/forum/viewtopic.php?t={topic_id}");
            let _ = self.navigate_and_wait(&topic_url, &init_cfg, 15_000)?;
            let req_id2 = self.next_id()?;
            let js2 = format!(
                r#"(function(){{
                    try {{
                        var h = document.documentElement ? document.documentElement.outerHTML : '';
                        window.ipc.postMessage(JSON.stringify({{ kind: 'topic_html', id: {req_id2}, ok: true, status: 200, html: h }}));
                    }} catch (e) {{
                        window.ipc.postMessage(JSON.stringify({{ kind: 'fetch_err', id: {req_id2}, error: String(e) }}));
                    }}
                }})();"#
            );
            if let Ok(WvFetchOutcome::TopicHtml { html, .. }) =
                self.run_fetch(&init_cfg, &base, js2, req_id2, 10)
            {
                if let Some(magnet) = extract_magnet_from_html(&html) {
                    return Ok(RuTrackerPayload::Magnet(magnet));
                }
            }

            Err(format!(
                "Не удалось скачать .torrent или получить magnet с темы RuTracker #{topic_id} через WebView2"
            ))
        }

        fn next_id(&self) -> Result<u64, String> {
            self.with_worker(|c| Ok(c.next_id()))
        }

        fn current_url(&self) -> Result<String, String> {
            self.with_worker(|client| {
                let (tx, rx) = mpsc::channel();
                client.send_cmd(WvCommand::GetUrl { reply: tx })?;
                rx.recv_timeout(Duration::from_secs(3))
                    .map_err(|_| "Таймаут получения URL WebView2".to_string())
            })
        }

        fn show_window_at(&self, url: &str, init_cfg: &WvInitConfig) -> Result<(), String> {
            let u = url.to_string();
            let cfg_c = init_cfg.clone();
            self.with_worker(|client| {
                let (tx, rx) = mpsc::channel();
                client.send_cmd(WvCommand::OpenLoginWindow {
                    url: u,
                    init_cfg: cfg_c,
                    on_status: None,
                    reply: tx,
                })?;
                let _ = rx.recv_timeout(Duration::from_secs(10));
                Ok(())
            })
        }

        fn navigate_and_wait(
            &self,
            url: &str,
            init_cfg: &WvInitConfig,
            timeout_ms: u64,
        ) -> Result<PageReadyData, String> {
            let u = url.to_string();
            let cfg_c = init_cfg.clone();
            self.with_worker(|client| {
                let (tx, rx) = mpsc::channel();
                client.send_cmd(WvCommand::NavigateAndWait {
                    url: u,
                    init_cfg: cfg_c,
                    timeout_ms,
                    reply: tx,
                })?;
                rx.recv_timeout(Duration::from_millis(timeout_ms + 3000))
                    .map_err(|_| "Таймаут ожидания страницы RuTracker в WebView2".to_string())?
            })
        }

        fn run_fetch(
            &self,
            init_cfg: &WvInitConfig,
            base_url: &str,
            js: String,
            req_id: u64,
            timeout_secs: u64,
        ) -> Result<WvFetchOutcome, String> {
            let cfg_c = init_cfg.clone();
            let b = base_url.to_string();
            self.with_worker(|client| {
                let (tx, rx) = mpsc::channel();
                client.send_cmd(WvCommand::RunFetch {
                    init_cfg: cfg_c,
                    base_url: b,
                    js,
                    req_id,
                    reply: tx,
                })?;
                rx.recv_timeout(Duration::from_secs(timeout_secs))
                    .map_err(|_| "Таймаут выполнения запроса в WebView2".to_string())
            })
        }
    }

    fn make_init_cfg(cfg: &TorrServerConfig) -> WvInitConfig {
        let user_agent = if cfg.rutracker_user_agent.trim().is_empty() {
            String::new()
        } else {
            cfg.effective_rutracker_user_agent()
        };
        WvInitConfig {
            user_agent,
            proxy: cfg.rutracker_proxy.trim().to_string(),
        }
    }

    fn looks_like_cloudflare_html(html: &str) -> bool {
        let low = html.to_lowercase();
        low.contains("just a moment")
            || low.contains("cf-browser-verification")
            || low.contains("cf-turnstile")
            || (low.contains("cloudflare") && low.contains("attention required"))
    }

    fn parse_wry_proxy(raw: &str) -> Option<ProxyConfig> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let (is_socks, rest) = if let Some(r) = trimmed.strip_prefix("socks5h://") {
            (true, r)
        } else if let Some(r) = trimmed.strip_prefix("socks5://") {
            (true, r)
        } else if let Some(r) = trimmed.strip_prefix("http://") {
            (false, r)
        } else if let Some(r) = trimmed.strip_prefix("https://") {
            (false, r)
        } else {
            (false, trimmed)
        };
        let host_port = rest.split('/').next().unwrap_or(rest);
        let host_port = host_port.rsplit('@').next().unwrap_or(host_port);
        let (host, port) = host_port.rsplit_once(':')?;
        let host = host.trim().to_string();
        let port = port.trim().to_string();
        if host.is_empty() || port.is_empty() {
            return None;
        }
        let ep = ProxyEndpoint { host, port };
        Some(if is_socks {
            ProxyConfig::Socks5(ep)
        } else {
            ProxyConfig::Http(ep)
        })
    }

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn spawn_wv_worker(webview_dir: PathBuf) -> Result<WorkerClient, String> {
        let cmd_queue = Arc::new(Mutex::new(VecDeque::new()));
        let queue_worker = Arc::clone(&cmd_queue);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<isize, String>>();

        let join_handle = thread::Builder::new()
            .name("ts-rutracker-wv2".to_string())
            .spawn(move || unsafe {
                let _ = CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED);

                if let Err(e) = fs::create_dir_all(&webview_dir) {
                    let _ = ready_tx.send(Err(format!(
                        "Не удалось создать каталог WebView2 {}: {e}",
                        webview_dir.display()
                    )));
                    CoUninitialize();
                    return;
                }

                let hinstance = GetModuleHandleW(std::ptr::null());
                let class_w = to_wide("TorrServerUI_RuTracker_WV2_Class");
                let title_w = to_wide("RuTracker — Вход и проверка Cloudflare");

                let mut wc: WNDCLASSW = std::mem::zeroed();
                wc.style = CS_HREDRAW | CS_VREDRAW;
                wc.lpfnWndProc = Some(wv_wndproc);
                wc.hInstance = hinstance;
                wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
                wc.hbrBackground = ((COLOR_WINDOW + 1) as usize) as HBRUSH;
                wc.lpszClassName = class_w.as_ptr();
                RegisterClassW(&wc);

                let hwnd = CreateWindowExW(
                    0,
                    class_w.as_ptr(),
                    title_w.as_ptr(),
                    WS_OVERLAPPEDWINDOW,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    1020,
                    740,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    hinstance,
                    std::ptr::null(),
                );

                if hwnd.is_null() {
                    let _ = ready_tx.send(Err("Не удалось создать окно WebView2".to_string()));
                    CoUninitialize();
                    return;
                }

                let web_context = WebContext::new(Some(webview_dir.clone()));
                WV_STATE.with(|cell| {
                    *cell.borrow_mut() = Some(WvThreadState {
                        webview_dir,
                        hwnd_raw: hwnd as isize,
                        hinstance_raw: hinstance as isize,
                        web_context,
                        webview: None,
                        config_sig: String::new(),
                        pending_fetches: Arc::new(Mutex::new(HashMap::new())),
                        pending_nav: None,
                        interactive_login_open: false,
                        saw_login_or_cf_during_popup: false,
                        login_status_cb: None,
                        cmd_queue: queue_worker,
                    });
                });

                let _ = ready_tx.send(Ok(hwnd as isize));

                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                WV_STATE.with(|cell| {
                    *cell.borrow_mut() = None;
                });
                CoUninitialize();
            })
            .map_err(|e| format!("Не удалось запустить поток WebView2: {e}"))?;

        let hwnd_raw = ready_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "Таймаут запуска потока WebView2".to_string())??;

        Ok(WorkerClient {
            hwnd_raw,
            cmd_queue,
            next_req_id: std::sync::atomic::AtomicU64::new(1),
            join_handle: Some(join_handle),
        })
    }

    fn ensure_webview(init_cfg: &WvInitConfig, initial_url: &str) -> Result<(Rc<WebView>, isize), String> {
        let new_sig = init_cfg.signature();

        let existing = WV_STATE.with(|cell| {
            let borrow = cell.borrow();
            let st = borrow.as_ref()?;
            if let Some(wv) = st.webview.as_ref() {
                if st.config_sig == new_sig {
                    return Some((Rc::clone(wv), st.hwnd_raw));
                }
            }
            None
        });
        if let Some(res) = existing {
            return Ok(res);
        }

        // Extract resources needed to build (or rebuild) WebView without holding RefCell borrow across COM call
        enum BuildPrep {
            SameHwnd(isize, isize),
            RecreateHwnd(isize, isize, PathBuf),
        }

        let (prep, pending_fetches) = WV_STATE.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let st = borrow.as_mut().expect("WvThreadState missing");
            let pf = Arc::clone(&st.pending_fetches);
            let had_old = st.webview.take().is_some();
            if had_old {
                // Recreate HWND cleanly when proxy/user-agent config changed
                let old_hwnd = st.hwnd_raw;
                let hinst = st.hinstance_raw;
                let dir = st.webview_dir.clone();
                st.hwnd_raw = 0;
                (BuildPrep::RecreateHwnd(old_hwnd, hinst, dir), pf)
            } else {
                (BuildPrep::SameHwnd(st.hwnd_raw, st.hinstance_raw), pf)
            }
        });

        let (hwnd_raw, hinstance_raw) = match prep {
            BuildPrep::SameHwnd(h, hi) => (h, hi),
            BuildPrep::RecreateHwnd(old_h, hi, _dir) => unsafe {
                if old_h != 0 {
                    DestroyWindow(old_h as HWND);
                }
                let class_w = to_wide("TorrServerUI_RuTracker_WV2_Class");
                let title_w = to_wide("RuTracker — Вход и проверка Cloudflare");
                let new_hwnd = CreateWindowExW(
                    0,
                    class_w.as_ptr(),
                    title_w.as_ptr(),
                    WS_OVERLAPPEDWINDOW,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    1020,
                    740,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    hi as _,
                    std::ptr::null(),
                );
                if new_hwnd.is_null() {
                    return Err("Не удалось пересоздать окно WebView2".to_string());
                }
                WV_STATE.with(|cell| {
                    if let Some(st) = cell.borrow_mut().as_mut() {
                        st.hwnd_raw = new_hwnd as isize;
                    }
                });
                (new_hwnd as isize, hi)
            },
        };

        let nz_hwnd = NonZeroIsize::new(hwnd_raw)
            .ok_or_else(|| "Некорректный HWND WebView2".to_string())?;
        let raw_win = RawWin32Window {
            hwnd: nz_hwnd,
            hinstance: NonZeroIsize::new(hinstance_raw),
        };

        let pf_ipc = Arc::clone(&pending_fetches);
        let hwnd_for_load = hwnd_raw;

        // Temporarily take WebContext out of WV_STATE to build WebView without holding RefCell borrow
        let mut temp_ctx = WV_STATE.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let st = borrow.as_mut().expect("WvThreadState missing");
            std::mem::replace(&mut st.web_context, WebContext::new(None))
        });

        let mut builder = WebViewBuilder::new_with_web_context(&mut temp_ctx)
            .with_url(initial_url)
            .with_ipc_handler(move |req| {
                handle_ipc_message(req.body(), &pf_ipc);
            })
            .with_on_page_load_handler(move |event, _url| {
                if matches!(event, PageLoadEvent::Finished) {
                    unsafe {
                        PostMessageW(hwnd_for_load as HWND, WM_WV_PAGE_FINISHED, 0, 0);
                    }
                }
            });

        if !init_cfg.user_agent.trim().is_empty() {
            builder = builder.with_user_agent(init_cfg.user_agent.trim());
        }
        if let Some(proxy_cfg) = parse_wry_proxy(&init_cfg.proxy) {
            builder = builder.with_proxy_config(proxy_cfg);
        }

        let built = builder.build(&raw_win);

        // Restore WebContext back into WV_STATE
        WV_STATE.with(|cell| {
            if let Some(st) = cell.borrow_mut().as_mut() {
                st.web_context = temp_ctx;
            }
        });

        let webview = built.map_err(|e| {
            format!(
                "Не удалось запустить Edge WebView2 (проверьте наличие WebView2 Runtime в Windows): {e}"
            )
        })?;
        let wv_rc = Rc::new(webview);

        WV_STATE.with(|cell| {
            if let Some(st) = cell.borrow_mut().as_mut() {
                st.webview = Some(Rc::clone(&wv_rc));
                st.config_sig = new_sig;
            }
        });

        diagnostics::info("[TS:RUTRACKER]", "Встроенный движок Edge WebView2 инициализирован");
        Ok((wv_rc, hwnd_raw))
    }

    fn handle_ipc_message(
        raw: &str,
        pending_fetches: &Arc<Mutex<HashMap<u64, mpsc::Sender<WvFetchOutcome>>>>,
    ) {
        let Ok(msg) = serde_json::from_str::<WvIpcMessage>(raw) else {
            return;
        };

        match msg {
            WvIpcMessage::FetchHtml { id, ok, status, html } => {
                if let Ok(mut map) = pending_fetches.lock() {
                    if let Some(tx) = map.remove(&id) {
                        let _ = tx.send(WvFetchOutcome::Html { ok, status, html });
                    }
                }
            }
            WvIpcMessage::Torrent { id, b64 } => {
                let outcome = match decode_base64(&b64) {
                    Ok(bytes) => WvFetchOutcome::TorrentBytes(bytes),
                    Err(e) => WvFetchOutcome::Error(e),
                };
                if let Ok(mut map) = pending_fetches.lock() {
                    if let Some(tx) = map.remove(&id) {
                        let _ = tx.send(outcome);
                    }
                }
            }
            WvIpcMessage::TopicHtml { id, ok, status, html } => {
                if let Ok(mut map) = pending_fetches.lock() {
                    if let Some(tx) = map.remove(&id) {
                        let _ = tx.send(WvFetchOutcome::TopicHtml { ok, status, html });
                    }
                }
            }
            WvIpcMessage::FetchErr { id, error } => {
                if let Ok(mut map) = pending_fetches.lock() {
                    if let Some(tx) = map.remove(&id) {
                        let _ = tx.send(WvFetchOutcome::Error(error));
                    }
                }
            }
            WvIpcMessage::PageReady {
                url,
                has_cf,
                logged_in,
                has_login_form,
                has_results,
                user,
                html,
            } => {
                let mut status_cb_to_call: Option<(Arc<dyn Fn(String) + Send + Sync>, String)> =
                    None;
                let mut nav_to_complete: Option<(PendingNav, PageReadyData)> = None;
                let mut hwnd_to_update: Option<(isize, bool)> = None;

                WV_STATE.with(|cell| {
                    let mut borrow = cell.borrow_mut();
                    let Some(st) = borrow.as_mut() else {
                        return;
                    };

                    if st.interactive_login_open {
                        if has_cf || !logged_in {
                            st.saw_login_or_cf_during_popup = true;
                        }
                        if !has_cf && logged_in {
                            let who = if user.is_empty() {
                                String::new()
                            } else {
                                format!(": {user}")
                            };
                            let msg = format!("Вход через WebView2 активен{who}");
                            if let Some(cb) = st.login_status_cb.as_ref() {
                                status_cb_to_call = Some((Arc::clone(cb), msg));
                            }
                            if st.saw_login_or_cf_during_popup {
                                st.saw_login_or_cf_during_popup = false;
                                st.interactive_login_open = false;
                                hwnd_to_update = Some((st.hwnd_raw, true));
                            } else {
                                hwnd_to_update = Some((st.hwnd_raw, false));
                            }
                        }
                    }

                    if let Some(mut nav) = st.pending_nav.take() {
                        if !url.starts_with("http") {
                            st.pending_nav = Some(nav);
                        } else if has_cf {
                            nav.saw_cf = true;
                            st.pending_nav = Some(nav);
                        } else {
                            nav_to_complete = Some((
                                nav,
                                PageReadyData {
                                    _url: url.clone(),
                                    logged_in,
                                    has_login_form,
                                    _has_results: has_results,
                                    user: user.clone(),
                                    html: html.clone(),
                                },
                            ));
                        }
                    }
                });

                if let Some((hwnd_raw, should_hide)) = hwnd_to_update {
                    unsafe {
                        let title = to_wide("RuTracker — Вход выполнен!");
                        SetWindowTextW(hwnd_raw as HWND, title.as_ptr());
                        if should_hide {
                            ShowWindow(hwnd_raw as HWND, SW_HIDE);
                        }
                    }
                }

                if let Some((cb, msg)) = status_cb_to_call {
                    cb(msg);
                }

                if let Some((nav, data)) = nav_to_complete {
                    unsafe {
                        let hwnd_raw = WV_STATE
                            .with(|c| c.borrow().as_ref().map(|s| s.hwnd_raw).unwrap_or(0));
                        if hwnd_raw != 0 {
                            KillTimer(hwnd_raw as HWND, TIMER_NAV_ID);
                        }
                    }
                    let _ = nav.reply.send(Ok(data));
                }
            }
        }
    }

    fn process_queued_commands() {
        let cmds: Vec<WvCommand> = WV_STATE.with(|cell| {
            let borrow = cell.borrow();
            let Some(st) = borrow.as_ref() else {
                return Vec::new();
            };
            let mut out = Vec::new();
            if let Ok(mut q) = st.cmd_queue.lock() {
                while let Some(c) = q.pop_front() {
                    out.push(c);
                }
            }
            out
        });

        for cmd in cmds {
            match cmd {
                WvCommand::OpenLoginWindow {
                    url,
                    init_cfg,
                    on_status,
                    reply,
                } => {
                    let res = (|| -> Result<(), String> {
                        let (wv, hwnd_raw) = ensure_webview(&init_cfg, "about:blank")?;
                        WV_STATE.with(|cell| {
                            if let Some(st) = cell.borrow_mut().as_mut() {
                                st.interactive_login_open = true;
                                st.saw_login_or_cf_during_popup = false;
                                if let Some(cb) = on_status {
                                    st.login_status_cb = Some(cb);
                                }
                            }
                        });
                        let _ = wv.load_url(&url);
                        unsafe {
                            let title = to_wide("RuTracker — Вход и проверка Cloudflare (закройте окно после входа)");
                            SetWindowTextW(hwnd_raw as HWND, title.as_ptr());
                            ShowWindow(hwnd_raw as HWND, SW_RESTORE);
                            ShowWindow(hwnd_raw as HWND, SW_SHOW);
                            SetForegroundWindow(hwnd_raw as HWND);
                        }
                        Ok(())
                    })();
                    let _ = reply.send(res);
                }
                WvCommand::ShowWindow => {
                    let hwnd_raw = WV_STATE.with(|cell| {
                        if let Some(st) = cell.borrow_mut().as_mut() {
                            st.interactive_login_open = true;
                            st.saw_login_or_cf_during_popup = true;
                            st.hwnd_raw
                        } else {
                            0
                        }
                    });
                    if hwnd_raw != 0 {
                        unsafe {
                            ShowWindow(hwnd_raw as HWND, SW_RESTORE);
                            ShowWindow(hwnd_raw as HWND, SW_SHOW);
                            SetForegroundWindow(hwnd_raw as HWND);
                        }
                    }
                }
                WvCommand::NavigateAndWait {
                    url,
                    init_cfg,
                    timeout_ms,
                    reply,
                } => {
                    match ensure_webview(&init_cfg, "about:blank") {
                        Ok((wv, hwnd_raw)) => {
                            let old_nav = WV_STATE.with(|cell| {
                                let mut borrow = cell.borrow_mut();
                                let st = borrow.as_mut()?;
                                st.pending_nav.replace(PendingNav {
                                    reply,
                                    deadline: Instant::now() + Duration::from_millis(timeout_ms),
                                    saw_cf: false,
                                })
                            });
                            if let Some(prev) = old_nav {
                                let _ = prev.reply.send(Err("Навигация прервана новым запросом".to_string()));
                            }
                            unsafe {
                                SetTimer(hwnd_raw as HWND, TIMER_NAV_ID, 500, None);
                            }
                            if let Err(e) = wv.load_url(&url) {
                                let aborted = WV_STATE.with(|c| {
                                    c.borrow_mut().as_mut().and_then(|s| s.pending_nav.take())
                                });
                                unsafe {
                                    KillTimer(hwnd_raw as HWND, TIMER_NAV_ID);
                                }
                                if let Some(nav) = aborted {
                                    let _ = nav.reply.send(Err(format!("Ошибка загрузки URL в WebView2: {e}")));
                                }
                            }
                        }
                        Err(e) => {
                            let _ = reply.send(Err(e));
                        }
                    }
                }
                WvCommand::RunFetch {
                    init_cfg,
                    base_url,
                    js,
                    req_id,
                    reply,
                } => {
                    match ensure_webview(&init_cfg, "about:blank") {
                        Ok((wv, _hwnd)) => {
                            let cur = wv.url().unwrap_or_default();
                            if !cur.starts_with(&base_url) {
                                let _ = reply.send(WvFetchOutcome::TopicHtml {
                                    ok: false,
                                    status: 403,
                                    html: String::new(),
                                });
                                continue;
                            }
                            let pf = WV_STATE.with(|c| {
                                c.borrow().as_ref().map(|s| Arc::clone(&s.pending_fetches))
                            });
                            if let Some(map_arc) = pf {
                                if let Ok(mut map) = map_arc.lock() {
                                    map.insert(req_id, reply);
                                }
                            }
                            if let Err(e) = wv.evaluate_script(&js) {
                                if let Some(map_arc) = WV_STATE.with(|c| {
                                    c.borrow().as_ref().map(|s| Arc::clone(&s.pending_fetches))
                                }) {
                                    if let Ok(mut map) = map_arc.lock() {
                                        if let Some(tx) = map.remove(&req_id) {
                                            let _ = tx.send(WvFetchOutcome::Error(format!(
                                                "Ошибка JS в WebView2: {e}"
                                            )));
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            let _ = reply.send(WvFetchOutcome::Error(e));
                        }
                    }
                }
                WvCommand::GetUrl { reply } => {
                    let wv_opt = WV_STATE.with(|c| {
                        c.borrow()
                            .as_ref()
                            .and_then(|s| s.webview.as_ref().map(Rc::clone))
                    });
                    let url = wv_opt
                        .and_then(|w| w.url().ok())
                        .unwrap_or_default();
                    let _ = reply.send(url);
                }
            }
        }
    }

    unsafe extern "system" fn wv_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_WV_CMD {
            process_queued_commands();
            return 0;
        }

        if msg == WM_WV_PAGE_FINISHED {
            let wv_opt = WV_STATE.with(|c| {
                c.borrow()
                    .as_ref()
                    .and_then(|s| s.webview.as_ref().map(Rc::clone))
            });
            if let Some(wv) = wv_opt {
                let _ = wv.evaluate_script(PAGE_PROBE_JS);
            }
            return 0;
        }

        if msg == WM_TIMER && wparam == TIMER_NAV_ID {
            let timed_out = WV_STATE.with(|cell| {
                let mut borrow = cell.borrow_mut();
                let st = borrow.as_mut()?;
                let expired = st
                    .pending_nav
                    .as_ref()
                    .map(|n| Instant::now() >= n.deadline)
                    .unwrap_or(false);
                if expired {
                    let nav = st.pending_nav.take()?;
                    if nav.saw_cf {
                        st.interactive_login_open = true;
                        st.saw_login_or_cf_during_popup = true;
                    }
                    Some((nav, st.hwnd_raw))
                } else {
                    None
                }
            });
            if let Some((nav, hwnd_raw)) = timed_out {
                KillTimer(hwnd, TIMER_NAV_ID);
                if nav.saw_cf {
                    ShowWindow(hwnd_raw as HWND, SW_RESTORE);
                    ShowWindow(hwnd_raw as HWND, SW_SHOW);
                    SetForegroundWindow(hwnd_raw as HWND);
                    let _ = nav.reply.send(Err(
                        "Cloudflare требует подтверждения: отметьте проверку в открывшемся окне браузера и повторите поиск.".to_string(),
                    ));
                } else {
                    let _ = nav.reply.send(Err(
                        "Таймаут загрузки страницы RuTracker в WebView2".to_string(),
                    ));
                }
            }
            return 0;
        }

        if msg == WM_CLOSE {
            WV_STATE.with(|cell| {
                if let Some(st) = cell.borrow_mut().as_mut() {
                    st.interactive_login_open = false;
                }
            });
            ShowWindow(hwnd, SW_HIDE);
            return 0;
        }

        if msg == WM_WV_QUIT {
            WV_STATE.with(|cell| {
                if let Some(st) = cell.borrow_mut().as_mut() {
                    st.webview = None;
                }
            });
            DestroyWindow(hwnd);
            PostQuitMessage(0);
            return 0;
        }

        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(input.len() * 3 / 4);
        let mut buf: u32 = 0;
        let mut bits: u8 = 0;

        for b in input.bytes() {
            let val = match b {
                b'A'..=b'Z' => b - b'A',
                b'a'..=b'z' => b - b'a' + 26,
                b'0'..=b'9' => b - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => break,
                b' ' | b'\n' | b'\r' | b'\t' => continue,
                _ => return Err("Некорректный символ base64 от WebView2".to_string()),
            };
            buf = (buf << 6) | (val as u32);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push(((buf >> bits) & 0xFF) as u8);
            }
        }
        Ok(out)
    }
}

#[cfg(not(windows))]
mod non_win_impl {
    use super::*;

    pub struct RuTrackerWebView;

    impl RuTrackerWebView {
        pub fn new(_webview_dir: PathBuf) -> Self {
            Self
        }

        pub fn shutdown(&self) {}

        pub fn open_login_window<F>(
            &self,
            _cfg: &TorrServerConfig,
            _on_status: F,
        ) -> Result<(), String>
        where
            F: Fn(String) + Send + Sync + 'static,
        {
            Err("Встроенный браузер WebView2 поддерживается только на Windows".to_string())
        }

        pub fn verify_login(&self, _cfg: &TorrServerConfig) -> Result<String, String> {
            Err("Встроенный браузер WebView2 поддерживается только на Windows".to_string())
        }

        pub fn search_tracker_path(
            &self,
            _cfg: &TorrServerConfig,
            _rel_path: &str,
            _log_label: &str,
        ) -> Result<Vec<ApiSearchResult>, String> {
            Err("Встроенный браузер WebView2 поддерживается только на Windows".to_string())
        }

        pub fn fetch_torrent_or_magnet(
            &self,
            _cfg: &TorrServerConfig,
            _topic_id: &str,
        ) -> Result<RuTrackerPayload, String> {
            Err("Встроенный браузер WebView2 поддерживается только на Windows".to_string())
        }
    }
}

#[cfg(windows)]
pub use win_impl::RuTrackerWebView;
#[cfg(not(windows))]
pub use non_win_impl::RuTrackerWebView;
