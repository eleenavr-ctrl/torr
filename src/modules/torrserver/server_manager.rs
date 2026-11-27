use crate::core::diagnostics;
use crate::core::storage::{PortablePaths, TorrServerConfig};
use crate::modules::torrserver::api::{format_speed, TorrServerApi};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::error::Error as StdError;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct GitHubRelease {
    tag_name: String,
    name: String,
    prerelease: bool,
    draft: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone)]
pub struct MatrixReleaseOption {
    pub tag: String,
    pub label: String,
    pub download_url: String,
    pub prerelease: bool,
}

pub fn default_matrix_releases() -> Vec<MatrixReleaseOption> {
    let tags = [
        ("MatriX.141", "MatriX.141 (рекомендуемый стабильный)"),
        ("MatriX.136", "MatriX.136"),
        ("MatriX.135", "MatriX.135"),
        ("MatriX.134", "MatriX.134"),
        ("MatriX.133", "MatriX.133"),
    ];
    tags.into_iter()
        .map(|(tag, label)| MatrixReleaseOption {
            tag: tag.to_string(),
            label: label.to_string(),
            download_url: format!(
                "https://github.com/YouROK/TorrServer/releases/download/{tag}/TorrServer-windows-amd64.exe"
            ),
            prerelease: false,
        })
        .collect()
}

pub fn fetch_available_matrix_releases() -> Result<Vec<MatrixReleaseOption>, String> {
    let api_client = Client::builder()
        .connect_timeout(Duration::from_secs(12))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("Ошибка HTTP-клиента GitHub: {e}"))?;

    let releases = api_client
        .get("https://api.github.com/repos/YouROK/TorrServer/releases?per_page=35")
        .header("User-Agent", "TorrServerUI/0.1.0")
        .send()
        .map_err(|e| format!("Ошибка запроса к GitHub API: {}", format_error_chain(&e)))?
        .error_for_status()
        .map_err(|e| format!("GitHub API вернул ошибку: {}", format_error_chain(&e)))?
        .json::<Vec<GitHubRelease>>()
        .map_err(|e| format!("Ошибка разбора релизов GitHub: {e}"))?;

    let mut out = Vec::new();
    let mut marked_latest_stable = false;
    for r in releases {
        if r.draft || !r.tag_name.to_lowercase().starts_with("matrix") {
            continue;
        }
        if let Some(asset) = r
            .assets
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case("TorrServer-windows-amd64.exe"))
            .or_else(|| {
                r.assets.iter().find(|a| {
                    let low = a.name.to_lowercase();
                    low.contains("windows")
                        && low.contains("amd64")
                        && low.ends_with(".exe")
                        && !low.contains("gst")
                })
            })
        {
            let label = if r.prerelease {
                format!("{} (pre-release)", r.tag_name)
            } else if !marked_latest_stable {
                marked_latest_stable = true;
                format!("{} (последний стабильный)", r.tag_name)
            } else {
                r.tag_name.clone()
            };
            out.push(MatrixReleaseOption {
                tag: r.tag_name,
                label,
                download_url: asset.browser_download_url.clone(),
                prerelease: r.prerelease,
            });
        }
    }

    if out.is_empty() {
        return Err("В релизах YouROK/TorrServer не найдено сборок MatriX для Windows x64".to_string());
    }
    Ok(out)
}

pub struct ServerManager {
    paths: PortablePaths,
    child: Mutex<Option<Child>>,
}

impl ServerManager {
    pub fn new(paths: PortablePaths) -> Self {
        Self {
            paths,
            child: Mutex::new(None),
        }
    }

    pub fn binary_exists(&self) -> bool {
        self.paths.torrserver_exe.exists()
    }

    /// Returns `Some(true)` if we own a running child, `Some(false)` if owned child exited,
    /// `None` if no child was spawned or attached.
    pub fn child_running_state(&self) -> Option<bool> {
        let mut guard = self.child.lock().ok()?;
        let ch = guard.as_mut()?;
        match ch.try_wait() {
            Ok(None) => Some(true),
            Ok(Some(_)) | Err(_) => {
                *guard = None;
                Some(false)
            }
        }
    }

    pub fn apply_child_priority(&self, above_normal: bool) {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Foundation::HANDLE;
            use windows_sys::Win32::System::Threading::{
                SetPriorityClass, ABOVE_NORMAL_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
            };

            if let Ok(mut guard) = self.child.lock() {
                if let Some(ch) = guard.as_mut() {
                    if let Ok(None) = ch.try_wait() {
                        let class = if above_normal {
                            ABOVE_NORMAL_PRIORITY_CLASS
                        } else {
                            NORMAL_PRIORITY_CLASS
                        };
                        unsafe {
                            let _ = SetPriorityClass(ch.as_raw_handle() as HANDLE, class);
                        }
                    }
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = above_normal;
        }
    }

    pub fn start(&self, cfg: &TorrServerConfig, high_priority: bool) -> Result<(), String> {
        if cfg.mode == 1 {
            diagnostics::info(
                "[TS:ENGINE]",
                format!("Выбран удалённый режим ({})", cfg.effective_base_url()),
            );
            return Ok(());
        }

        if !self.binary_exists() {
            let msg = format!(
                "Бинарник {} не найден",
                self.paths.torrserver_exe.display()
            );
            diagnostics::warn("[TS:ENGINE]", &msg);
            return Err(msg);
        }

        // Check if already running under our Child handle
        if let Ok(mut guard) = self.child.lock() {
            if let Some(ch) = guard.as_mut() {
                if let Ok(None) = ch.try_wait() {
                    return Ok(());
                }
                *guard = None;
            }
        }

        // If a compatible MatriX instance is already running on the configured port
        // (e.g., kept alive via keep_engine_on_exit), reuse it cleanly instead of failing on port conflict.
        let probe_api = TorrServerApi::from_config(cfg);
        if let Ok((ver, _)) = probe_api.echo() {
            diagnostics::info(
                "[TS:ENGINE]",
                format!(
                    "Подключено к уже работающему TorrServer ({ver}) на порту {}",
                    cfg.port
                ),
            );
            return Ok(());
        }

        let port_str = cfg.port.to_string();
        let cache_str = self.paths.ts_cache_dir.to_string_lossy().to_string();
        let use_yaml_mode = detect_yaml_config_mode(&self.paths.torrserver_exe);

        let mut cmd = Command::new(&self.paths.torrserver_exe);
        if use_yaml_mode {
            diagnostics::warn(
                "[TS:ENGINE]",
                "Обнаружен TorrServer Silo (-c config.yaml) с изменённым REST API (/echo -> 404). Нажмите «Обновить с GitHub» для установки совместимого MatriX.",
            );
            let cfg_yaml = self.paths.torrserver_dir.join("config.yaml");
            if cfg_yaml.exists() {
                cmd.args(["-c", "config.yaml"]);
            }
            cmd.env("TS_PORT", &port_str)
                .env("TS_PATH", &cache_str);
        } else {
            // IMPORTANT: Do NOT pass --rdb! In TorrServer MatriX, --rdb forces read-only DB mode,
            // preventing added torrents and settings from persisting to config.db.
            cmd.args(["--port", &port_str, "--path", &cache_str]);
        }

        cmd.current_dir(&self.paths.torrserver_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let mut flags = 0x08000000u32; // CREATE_NO_WINDOW
            if high_priority {
                flags |= 0x00008000u32; // ABOVE_NORMAL_PRIORITY_CLASS
            }
            cmd.creation_flags(flags);
        }
        #[cfg(not(windows))]
        let _ = high_priority;

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Ошибка запуска TorrServer-windows-amd64.exe: {e}"))?;

        if let Some(stdout) = child.stdout.take() {
            thread::Builder::new()
                .name("ts-stdout".to_string())
                .spawn(move || {
                    let reader = BufReader::new(stdout);
                    for line in reader.lines().map_while(Result::ok) {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            diagnostics::info("[TS:ENGINE]", trimmed.to_string());
                        }
                    }
                })
                .ok();
        }

        if let Some(stderr) = child.stderr.take() {
            thread::Builder::new()
                .name("ts-stderr".to_string())
                .spawn(move || {
                    let reader = BufReader::new(stderr);
                    for line in reader.lines().map_while(Result::ok) {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let low = trimmed.to_lowercase();
                        if low.contains("panic:") || low.contains("fatal ") || low.contains("fatal:")
                        {
                            diagnostics::error("[TS:ENGINE]", trimmed.to_string());
                        } else if low.contains(" error")
                            || low.starts_with("error")
                            || low.contains("err:")
                            || low.contains("warn")
                            || low.contains("failed")
                            || low.contains("cannot ")
                        {
                            diagnostics::warn("[TS:ENGINE]", trimmed.to_string());
                        } else {
                            diagnostics::info("[TS:ENGINE]", trimmed.to_string());
                        }
                    }
                })
                .ok();
        }

        if let Ok(mut guard) = self.child.lock() {
            *guard = Some(child);
        }

        // Wait briefly for the HTTP API to start listening (up to ~1.2s, exits immediately once ready)
        for _ in 0..8 {
            if self.child_running_state() == Some(false) {
                break;
            }
            if probe_api.echo().is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(150));
        }

        diagnostics::info(
            "[TS:ENGINE]",
            format!(
                "Локальный движок запущен на порту {} (БД: {}, приоритет: {})",
                cfg.port,
                self.paths.ts_cache_dir.display(),
                if high_priority { "высокий" } else { "обычный" }
            ),
        );
        Ok(())
    }

    pub fn stop(&self, cfg: &TorrServerConfig) {
        if cfg.mode == 0 {
            let api = TorrServerApi::from_config(cfg);
            let _ = api.shutdown_server();
        }

        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut child) = guard.take() {
                for _ in 0..30 {
                    if let Ok(Some(_)) = child.try_wait() {
                        diagnostics::info("[TS:ENGINE]", "Движок штатно остановлен");
                        return;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                let _ = child.kill();
                let _ = child.wait();
                diagnostics::warn("[TS:ENGINE]", "Движок принудительно завершён (kill)");
                return;
            }
        }

        // Wait briefly in case an external/reattached local TorrServer is shutting down from /shutdown
        if cfg.mode == 0 {
            let api = TorrServerApi::from_config(cfg);
            for _ in 0..12 {
                if api.echo().is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(120));
            }
        }
    }

    pub fn restart(&self, cfg: &TorrServerConfig, high_priority: bool) -> Result<(), String> {
        diagnostics::info("[TS:ENGINE]", "Перезапуск TorrServer...");
        self.stop(cfg);
        thread::sleep(Duration::from_millis(250));
        self.start(cfg, high_priority)
    }

    pub fn update_from_github<F>(
        &self,
        cfg: &TorrServerConfig,
        high_priority: bool,
        target_tag: Option<&str>,
        known_releases: &[MatrixReleaseOption],
        on_progress: F,
    ) -> Result<String, String>
    where
        F: Fn(&str, f32),
    {
        let requested_tag = target_tag
            .map(str::trim)
            .filter(|t| !t.is_empty() && !t.eq_ignore_ascii_case("auto"));

        if let Some(tag) = requested_tag {
            on_progress(&format!("Стадия 1/4: подготовка версии {tag}..."), 0.02);
            diagnostics::info(
                "[TS:UPDATER]",
                format!("Выбрана установка конкретной версии MatriX: {tag}"),
            );
        } else {
            on_progress(
                "Стадия 1/4: поиск актуального релиза MatriX на GitHub...",
                0.02,
            );
            diagnostics::info(
                "[TS:UPDATER]",
                "Поиск актуального релиза MatriX в YouROK/TorrServer",
            );
        }

        let dl_client = Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(|e| format!("Ошибка HTTP-клиента загрузки: {e}"))?;

        let (tag_name, download_url) = if let Some(tag) = requested_tag {
            if let Some(found) = known_releases.iter().find(|r| r.tag == tag) {
                (found.tag.clone(), found.download_url.clone())
            } else {
                (
                    tag.to_string(),
                    format!(
                        "https://github.com/YouROK/TorrServer/releases/download/{tag}/TorrServer-windows-amd64.exe"
                    ),
                )
            }
        } else {
            let fetched = fetch_available_matrix_releases();
            match fetched {
                Ok(list) => {
                    if let Some(best) = list
                        .iter()
                        .find(|r| !r.prerelease)
                        .or_else(|| list.first())
                    {
                        (best.tag.clone(), best.download_url.clone())
                    } else {
                        (
                            "MatriX.141".to_string(),
                            "https://github.com/YouROK/TorrServer/releases/download/MatriX.141/TorrServer-windows-amd64.exe".to_string(),
                        )
                    }
                }
                Err(e) => {
                    diagnostics::warn(
                        "[TS:UPDATER]",
                        format!("{e} — используем резервную ссылку MatriX.141"),
                    );
                    (
                        "MatriX.141".to_string(),
                        "https://github.com/YouROK/TorrServer/releases/download/MatriX.141/TorrServer-windows-amd64.exe".to_string(),
                    )
                }
            }
        };

        let new_path = self.paths.torrserver_exe.with_extension("exe.new");
        let max_attempts = 3usize;
        let mut last_err = String::new();

        for attempt in 1..=max_attempts {
            on_progress(
                &format!(
                    "Стадия 2/4: подключение к CDN для {tag_name}{}...",
                    if attempt > 1 {
                        format!(" (попытка {attempt}/{max_attempts})")
                    } else {
                        String::new()
                    }
                ),
                0.04,
            );
            diagnostics::info(
                "[TS:UPDATER]",
                format!(
                    "Скачивание ({tag_name}, попытка {attempt}/{max_attempts}): {download_url}"
                ),
            );

            match stream_download_exe(&dl_client, &download_url, &new_path, &tag_name, &on_progress) {
                Ok(_) => {
                    last_err.clear();
                    break;
                }
                Err(e) => {
                    let _ = fs::remove_file(&new_path);
                    last_err = e;
                    diagnostics::warn(
                        "[TS:UPDATER]",
                        format!("Попытка {attempt}/{max_attempts} не удалась: {last_err}"),
                    );
                    if attempt < max_attempts {
                        on_progress(
                            &format!(
                                "Стадия 2/4: повтор подключения ({attempt}/{max_attempts})..."
                            ),
                            0.04,
                        );
                        thread::sleep(Duration::from_millis(900));
                    }
                }
            }
        }

        if !last_err.is_empty() {
            return Err(last_err);
        }

        on_progress("Стадия 3/4: остановка процесса и установка .exe...", 0.96);
        self.stop(cfg);
        thread::sleep(Duration::from_millis(350));

        if self.paths.torrserver_exe.exists() {
            fs::remove_file(&self.paths.torrserver_exe).map_err(|e| {
                format!("Не удалось удалить старый TorrServer-windows-amd64.exe (файл занят): {e}")
            })?;
        }
        fs::rename(&new_path, &self.paths.torrserver_exe)
            .map_err(|e| format!("Не удалось переименовать .new в .exe: {e}"))?;

        diagnostics::info(
            "[TS:UPDATER]",
            format!("Движок обновлён до версии {tag_name}"),
        );

        if cfg.mode == 0 {
            on_progress("Стадия 4/4: запуск TorrServer...", 0.99);
            self.start(cfg, high_priority)?;
        }

        on_progress(&format!("Готово: установлен TorrServer {tag_name}"), 1.0);
        Ok(tag_name)
    }

    pub fn install_local_exe(
        &self,
        cfg: &TorrServerConfig,
        source_path: &Path,
        high_priority: bool,
    ) -> Result<(), String> {
        if !source_path.exists() {
            return Err(format!("Файл не найден: {}", source_path.display()));
        }
        self.stop(cfg);
        thread::sleep(Duration::from_millis(300));

        if source_path != self.paths.torrserver_exe {
            fs::copy(source_path, &self.paths.torrserver_exe)
                .map_err(|e| format!("Не удалось скопировать движок в data/torrserver/: {e}"))?;
        }
        diagnostics::info(
            "[TS:ENGINE]",
            format!("Установлен локальный бинарник из {}", source_path.display()),
        );
        if cfg.mode == 0 {
            self.start(cfg, high_priority)?;
        }
        Ok(())
    }
}

fn stream_download_exe<F>(
    client: &Client,
    url: &str,
    dest_path: &Path,
    tag_name: &str,
    on_progress: &F,
) -> Result<(), String>
where
    F: Fn(&str, f32),
{
    let mut resp = client
        .get(url)
        .header("User-Agent", "TorrServerUI/0.1.0")
        .send()
        .map_err(|e| format!("Ошибка скачивания бинарника: {}", format_error_chain(&e)))?
        .error_for_status()
        .map_err(|e| format!("Ошибка HTTP при скачивании бинарника: {}", format_error_chain(&e)))?;

    let expected_len = resp.content_length();
    let mut file = fs::File::create(dest_path)
        .map_err(|e| format!("Не удалось создать временный файл .new: {e}"))?;

    let mut buf = [0u8; 65536];
    let mut total: u64 = 0;
    let mut header_checked = false;
    let start_time = Instant::now();
    let mut last_tick = Instant::now();
    let mut bytes_since_tick: u64 = 0;
    let mut smooth_bps: f64 = 0.0;

    loop {
        let n = resp
            .read(&mut buf)
            .map_err(|e| format!("Ошибка чтения потока файла (скачано {total} Б): {e}"))?;
        if n == 0 {
            break;
        }
        if !header_checked {
            if n < 2 || &buf[..2] != b"MZ" {
                return Err("Сервер вернул не Windows .exe (возможно, HTML-страницу блокировки/ошибки)".to_string());
            }
            header_checked = true;
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("Ошибка записи .new файла: {e}"))?;
        total += n as u64;
        bytes_since_tick += n as u64;

        let dt = last_tick.elapsed().as_secs_f64();
        if dt >= 0.25 {
            let inst_bps = bytes_since_tick as f64 / dt;
            smooth_bps = if smooth_bps <= 0.0 {
                inst_bps
            } else {
                smooth_bps * 0.65 + inst_bps * 0.35
            };
            last_tick = Instant::now();
            bytes_since_tick = 0;

            let speed_txt = format_speed(smooth_bps);
            let mb_done = total as f64 / (1024.0 * 1024.0);
            if let Some(exp) = expected_len.filter(|&v| v > 0) {
                let ratio = (total as f64 / exp as f64).clamp(0.0, 1.0) as f32;
                let pct = (ratio * 100.0).round() as u32;
                let mb_total = exp as f64 / (1024.0 * 1024.0);
                on_progress(
                    &format!(
                        "Стадия 2/4: скачивание {tag_name} — {pct}% ({mb_done:.1} / {mb_total:.1} МБ) • {speed_txt}"
                    ),
                    ratio,
                );
            } else {
                on_progress(
                    &format!(
                        "Стадия 2/4: скачивание {tag_name} — {mb_done:.1} МБ • {speed_txt}"
                    ),
                    0.5,
                );
            }
        }
    }

    file.flush()
        .map_err(|e| format!("Ошибка сброса буфера .new файла: {e}"))?;

    if total < 1024 * 1024 {
        return Err(format!(
            "Скачанный файл слишком мал ({total} байт) — повреждён или заблокирован"
        ));
    }
    if let Some(exp) = expected_len.filter(|&v| v > 0) {
        if total != exp {
            return Err(format!(
                "Загрузка оборвалась: получено {total} из {exp} байт"
            ));
        }
    }

    let avg_bps = total as f64 / start_time.elapsed().as_secs_f64().max(0.05);
    let mb_total = total as f64 / (1024.0 * 1024.0);
    on_progress(
        &format!(
            "Стадия 2/4: скачано 100% ({mb_total:.1} МБ) • ср. скорость {}",
            format_speed(avg_bps)
        ),
        0.95,
    );
    Ok(())
}

fn detect_yaml_config_mode(exe_path: &Path) -> bool {
    let mut cmd = Command::new(exe_path);
    cmd.arg("-h")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let Ok(output) = cmd.output() else {
        return false;
    };
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&output.stderr));

    let low = text.to_lowercase();
    let has_port_flag = low.contains("--port") || low.contains("-port") || low.contains(" -p ");
    let has_yaml_flag = low.contains("config.yaml") || low.contains("yaml configuration");

    has_yaml_flag && !has_port_flag
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
