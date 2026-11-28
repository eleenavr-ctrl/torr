use crate::core::paths::AppPaths;
use crate::core::storage::{AppConfig, PortablePaths};
use chrono::Local;
use reqwest::blocking::Client;
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_LOG_ENTRIES: usize = 300;
const MAX_FILE_SIZE_BYTES: u64 = 1_048_576; // 1 MB rolling log

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticLevel {
    Info,
    Warn,
    Error,
}

impl DiagnosticLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            DiagnosticLevel::Info => "INFO",
            DiagnosticLevel::Warn => "WARN",
            DiagnosticLevel::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiagnosticEvent {
    pub timestamp: String,
    pub tag: String,
    pub level: DiagnosticLevel,
    pub message: String,
}

static LOG_BUFFER: OnceLock<Mutex<VecDeque<DiagnosticEvent>>> = OnceLock::new();
static LOG_FILE_PATH: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

fn buffer() -> &'static Mutex<VecDeque<DiagnosticEvent>> {
    LOG_BUFFER.get_or_init(|| Mutex::new(VecDeque::with_capacity(MAX_LOG_ENTRIES)))
}

pub fn init_logging(paths: &AppPaths) {
    let _ = LOG_FILE_PATH.get_or_init(|| {
        if paths.read_only {
            Mutex::new(None)
        } else {
            Mutex::new(Some(paths.logs_dir.join("torrserver-ui.log")))
        }
    });

    if !paths.read_only {
        let crash_path = paths.crash_log_path.clone();
        std::panic::set_hook(Box::new(move |panic_info| {
            let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let loc = panic_info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_else(|| "unknown".into());
            let payload = panic_info
                .payload()
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| panic_info.payload().downcast_ref::<String>().map(|s| s.as_str()))
                .unwrap_or("panic");
            let entry = format!("[{now}] PANIC at {loc}: {payload}\n");
            let _ = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&crash_path)
                .and_then(|mut f| f.write_all(entry.as_bytes()));
        }));
    }
}

pub fn crash_log_status(paths: &AppPaths) -> String {
    if paths.crash_log_path.exists() {
        let len = fs::metadata(&paths.crash_log_path).map(|m| m.len()).unwrap_or(0);
        format!("crash.log обнаружен ({len} Б)")
    } else {
        "crash.log отсутствует".to_string()
    }
}

fn append_rolling_log(line: &str) {
    let Some(lock) = LOG_FILE_PATH.get() else {
        return;
    };
    let Ok(guard) = lock.lock() else {
        return;
    };
    let Some(path) = guard.as_ref() else {
        return;
    };
    if let Ok(meta) = fs::metadata(path) {
        if meta.len() >= MAX_FILE_SIZE_BYTES {
            let p2 = path.with_extension("log.2");
            let p1 = path.with_extension("log.1");
            if p2.exists() {
                let _ = fs::remove_file(&p2);
            }
            if p1.exists() {
                let _ = fs::rename(&p1, &p2);
            }
            let _ = fs::rename(path, &p1);
        }
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

pub fn log_event(level: DiagnosticLevel, tag: &str, message: impl Into<String>) {
    let msg = message.into();
    let timestamp = Local::now().format("%H:%M:%S").to_string();
    let formatted = format!("[{timestamp}] [{}] {tag} {msg}", level.as_str());
    append_rolling_log(&formatted);
    match level {
        DiagnosticLevel::Info => tracing::info!(target: "app", "{tag} {msg}"),
        DiagnosticLevel::Warn => tracing::warn!(target: "app", "{tag} {msg}"),
        DiagnosticLevel::Error => tracing::error!(target: "app", "{tag} {msg}"),
    }
    let event = DiagnosticEvent {
        timestamp,
        tag: tag.to_string(),
        level,
        message: msg,
    };
    if let Ok(mut guard) = buffer().lock() {
        if guard.len() >= MAX_LOG_ENTRIES {
            guard.pop_front();
        }
        guard.push_back(event);
    }
}

pub fn info(tag: &str, message: impl Into<String>) {
    log_event(DiagnosticLevel::Info, tag, message);
}

pub fn warn(tag: &str, message: impl Into<String>) {
    log_event(DiagnosticLevel::Warn, tag, message);
}

pub fn error(tag: &str, message: impl Into<String>) {
    log_event(DiagnosticLevel::Error, tag, message);
}

pub fn get_logs() -> Vec<DiagnosticEvent> {
    buffer()
        .lock()
        .map(|g| g.iter().cloned().collect())
        .unwrap_or_default()
}

pub fn clear_logs() {
    if let Ok(mut guard) = buffer().lock() {
        guard.clear();
    }
    info("[CORE]", "Журнал диагностики очищен");
}

#[derive(Debug, Clone)]
pub struct DiagnosticsReport {
    pub app_version: String,
    pub engine_version: String,
    pub binary_status: String,
    pub port_status: String,
    pub api_ping: String,
    pub problem_hint: String,
    pub has_problem: bool,
    pub modules_summary: String,
}

pub fn run_diagnostics(
    paths: &PortablePaths,
    config: &AppConfig,
    modules_snapshot: &str,
) -> DiagnosticsReport {
    let app_version = env!("CARGO_PKG_VERSION").to_string();
    let ts_cfg = &config.torrserver;

    let binary_exists = paths.torrserver_exe.exists();
    let binary_status = if binary_exists {
        match fs::metadata(&paths.torrserver_exe) {
            Ok(meta) => format!("Найден ({:.1} МБ)", meta.len() as f64 / 1_048_576.0),
            Err(_) => "Найден".to_string(),
        }
    } else {
        "Отсутствует в UserData/torrserver/".to_string()
    };

    let base_url = ts_cfg.effective_base_url();
    let client = Client::builder().timeout(Duration::from_millis(1800)).build();

    let mut engine_version = "Не отвечает".to_string();
    let mut api_ping = "—".to_string();
    let mut api_ok = false;
    let mut api_timeout = false;
    let mut api_silo_incompatible = false;

    if let Ok(cli) = client {
        let echo_url = format!("{base_url}/echo");
        let started = Instant::now();
        let mut req = cli.get(&echo_url);
        if !ts_cfg.username.is_empty() || !ts_cfg.password.is_empty() {
            req = req.basic_auth(&ts_cfg.username, Some(&ts_cfg.password));
        }
        match req.send() {
            Ok(resp) if resp.status().is_success() => {
                let ms = started.elapsed().as_millis();
                let ver = resp.text().unwrap_or_default().trim().to_string();
                engine_version = if ver.is_empty() { "Активен".into() } else { ver };
                api_ping = format!("{ms} мс");
                api_ok = true;
            }
            Ok(resp) => {
                let code = resp.status().as_u16();
                engine_version = format!("HTTP {code}");
                if code == 404 {
                    if let Ok(root_resp) = cli.get(&base_url).send() {
                        let body = root_resp.text().unwrap_or_default().to_lowercase();
                        if body.contains("silo") || body.contains("torrserver") {
                            api_silo_incompatible = true;
                            engine_version = "TorrServer Silo (несовместим)".into();
                        }
                    }
                }
            }
            Err(e) => api_timeout = e.is_timeout(),
        }
    }

    let port_status = if ts_cfg.mode == 1 {
        if api_ok {
            format!("Удалённый активен ({base_url})")
        } else {
            format!("Удалённый недоступен ({base_url})")
        }
    } else {
        let port = ts_cfg.port;
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        if api_ok {
            format!("Порт {port}: активен")
        } else if api_silo_incompatible {
            format!("Порт {port}: TorrServer Silo")
        } else if TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok() {
            format!("Порт {port}: занят")
        } else if TcpListener::bind(addr).is_ok() {
            format!("Порт {port}: свободен")
        } else {
            format!("Порт {port}: занят")
        }
    };

    let custom_player_missing = !ts_cfg.player_custom_path.trim().is_empty()
        && !Path::new(ts_cfg.player_custom_path.trim()).exists();

    let (has_problem, problem_hint) = if api_ok {
        if custom_player_missing {
            (true, format!("Не найден плеер: {}", ts_cfg.player_custom_path))
        } else {
            (false, "Всё работает штатно".to_string())
        }
    } else if ts_cfg.mode == 0 && !binary_exists {
        (true, "Файл движка отсутствует в UserData/torrserver/".to_string())
    } else if api_silo_incompatible {
        (true, "Запущен TorrServer Silo без /echo".to_string())
    } else if api_timeout {
        (true, "Таймаут подключения к TorrServer".to_string())
    } else {
        (true, "Движок TorrServer не запущен или недоступен".to_string())
    };

    DiagnosticsReport {
        app_version,
        engine_version,
        binary_status,
        port_status,
        api_ping,
        problem_hint,
        has_problem,
        modules_summary: modules_snapshot.to_string(),
    }
}

pub fn format_plain_report(report: &DiagnosticsReport) -> String {
    let logs = get_logs();
    let mut out = String::new();
    out.push_str("=== TorrServerUI Диагностический отчёт ===\n");
    out.push_str(&format!("Версия оболочки: v{}\n", report.app_version));
    out.push_str(&format!("Движок: {}\n", report.engine_version));
    out.push_str(&format!("Файл: {}\n", report.binary_status));
    out.push_str(&format!("Порт: {}\n", report.port_status));
    out.push_str(&format!("Ping API: {}\n", report.api_ping));
    out.push_str(&format!("Диагноз: {}\n\n", report.problem_hint));
    out.push_str("--- Модули ---\n");
    out.push_str(&report.modules_summary);
    out.push_str("\n\n--- Логи ---\n");
    for entry in logs {
        out.push_str(&format!(
            "[{}] [{}] {} {}\n",
            entry.timestamp,
            entry.level.as_str(),
            entry.tag,
            entry.message
        ));
    }
    out
}

pub fn format_ai_markdown(report: &DiagnosticsReport) -> String {
    let logs = get_logs();
    let mut warnings_errors = Vec::new();
    let mut full_log = String::new();
    for entry in &logs {
        let line = format!(
            "[{}] [{}] {} {}",
            entry.timestamp,
            entry.level.as_str(),
            entry.tag,
            entry.message
        );
        if matches!(entry.level, DiagnosticLevel::Warn | DiagnosticLevel::Error) {
            warnings_errors.push(line.clone());
        }
        full_log.push_str(&line);
        full_log.push('\n');
    }
    let mut md = String::new();
    md.push_str("# Снимок диагностики TorrServerUI для ИИ\n\n");
    md.push_str(&format!(
        "- **Сборка:** `TorrServerUI v{}` (`{}` / `{}`)\n",
        report.app_version, std::env::consts::OS, std::env::consts::ARCH
    ));
    md.push_str(&format!("- **Движок:** `{}`\n", report.engine_version));
    md.push_str(&format!("- **Порт:** `{}` | **Ping:** `{}`\n", report.port_status, report.api_ping));
    md.push_str(&format!("- **Итог:** {}\n\n```text\n{}\n```\n", report.problem_hint, report.modules_summary));
    if !warnings_errors.is_empty() {
        md.push_str("\n## Предупреждения и ошибки\n```log\n");
        for w in warnings_errors {
            md.push_str(&w);
            md.push('\n');
        }
        md.push_str("```\n");
    }
    md.push_str("\n## Полный лог\n```log\n");
    md.push_str(&full_log);
    md.push_str("```\n");
    md
}
