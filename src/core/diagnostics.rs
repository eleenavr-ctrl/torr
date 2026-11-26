use crate::core::storage::{AppConfig, PortablePaths};
use chrono::Local;
use reqwest::blocking::Client;
use std::collections::VecDeque;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_LOG_ENTRIES: usize = 300;

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

fn buffer() -> &'static Mutex<VecDeque<DiagnosticEvent>> {
    LOG_BUFFER.get_or_init(|| Mutex::new(VecDeque::with_capacity(MAX_LOG_ENTRIES)))
}

pub fn log_event(level: DiagnosticLevel, tag: &str, message: impl Into<String>) {
    let msg = message.into();
    let timestamp = Local::now().format("%H:%M:%S").to_string();
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

    // 1. Check engine binary on disk
    let binary_exists = paths.torrserver_exe.exists();
    let binary_status = if binary_exists {
        match std::fs::metadata(&paths.torrserver_exe) {
            Ok(meta) => {
                let mb = meta.len() as f64 / (1024.0 * 1024.0);
                format!("Найден ({mb:.1} МБ)")
            }
            Err(_) => "Найден".to_string(),
        }
    } else {
        "Отсутствует в data/torrserver/".to_string()
    };

    // 2. Check API /echo & Ping
    let base_url = ts_cfg.effective_base_url();
    let client = Client::builder()
        .timeout(Duration::from_millis(1800))
        .build();

    let mut engine_version = "Не отвечает".to_string();
    let mut api_ping = "—".to_string();
    let mut api_ok = false;
    let mut api_timeout = false;

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
                engine_version = if ver.is_empty() {
                    "Активен".to_string()
                } else {
                    ver
                };
                api_ping = format!("{ms} мс");
                api_ok = true;
            }
            Ok(resp) => {
                engine_version = format!("HTTP {}", resp.status().as_u16());
            }
            Err(e) => {
                if e.is_timeout() {
                    api_timeout = true;
                }
            }
        }
    }

    // 3. Check local TCP port status
    let port_status = if ts_cfg.mode == 1 {
        if api_ok {
            format!("Удалённый сервер активен ({base_url})")
        } else {
            format!("Удалённый сервер недоступен ({base_url})")
        }
    } else {
        let port = ts_cfg.port;
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        if api_ok {
            format!("Порт {port}: активен (TorrServer)")
        } else if TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok() {
            format!("Порт {port}: занят сторонним процессом")
        } else if TcpListener::bind(addr).is_ok() {
            format!("Порт {port}: свободен (движок остановлен)")
        } else {
            format!("Порт {port}: занят другим приложением")
        }
    };

    // 4. Player binary check (if custom path set)
    let custom_player_missing = !ts_cfg.player_custom_path.trim().is_empty()
        && !Path::new(ts_cfg.player_custom_path.trim()).exists();

    // 5. Human-readable problem diagnosis
    let (has_problem, problem_hint) = if api_ok {
        if custom_player_missing {
            (
                true,
                format!(
                    "Сервер работает, но не найден выбранный видеоплеер: {}",
                    ts_cfg.player_custom_path
                ),
            )
        } else if ts_cfg.mode == 0 && !binary_exists {
            (
                false,
                format!(
                    "Подключено к уже запущенному TorrServer ({engine_version}, порт {}). Для автозапуска оболочкой можно скачать движок в data/torrserver/.",
                    ts_cfg.port
                ),
            )
        } else {
            (false, "Всё работает штатно".to_string())
        }
    } else if ts_cfg.mode == 0 && !binary_exists {
        (
            true,
            "Файл движка не найден в data/torrserver/. Нажмите «Скачать движок» на вкладке Раздачи или в Настройках."
                .to_string(),
        )
    } else if custom_player_missing {
        (
            true,
            format!(
                "Не найден выбранный видеоплеер: {}",
                ts_cfg.player_custom_path
            ),
        )
    } else if ts_cfg.mode == 0 && port_status.contains("занят") {
        (
            true,
            format!(
                "Порт {} занят другим приложением. Смените порт в настройках TorrServer или закройте конфликтующую программу.",
                ts_cfg.port
            ),
        )
    } else if api_timeout {
        (
            true,
            "Таймаут подключения к серверу. Проверьте фаервол или статус движка.".to_string(),
        )
    } else {
        (
            true,
            if ts_cfg.mode == 0 {
                "Движок TorrServer не запущен или ещё инициализируется. Попробуйте перезапустить движок."
                    .to_string()
            } else {
                format!("Нет связи с удалённым TorrServer ({base_url}). Проверьте URL, логин и пароль.")
            },
        )
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
    out.push_str(&format!("Версия движка: {}\n", report.engine_version));
    out.push_str(&format!("Бинарник движка: {}\n", report.binary_status));
    out.push_str(&format!("Статус порта: {}\n", report.port_status));
    out.push_str(&format!("Ping API: {}\n", report.api_ping));
    out.push_str(&format!("Диагноз: {}\n\n", report.problem_hint));
    out.push_str("--- Модули ---\n");
    out.push_str(&report.modules_summary);
    out.push_str("\n\n--- Последние события ---\n");
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
    md.push_str("## Окружение и состояние\n");
    md.push_str(&format!("- **Сборка оболочки:** `TorrServerUI v{}` (`{}` / `{}`)\n",
        report.app_version, std::env::consts::OS, std::env::consts::ARCH));
    md.push_str(&format!("- **Движок TorrServer:** `{}`\n", report.engine_version));
    md.push_str(&format!("- **Файл движка:** `{}`\n", report.binary_status));
    md.push_str(&format!("- **TCP-порт:** `{}`\n", report.port_status));
    md.push_str(&format!("- **Ping API:** `{}`\n", report.api_ping));
    md.push_str(&format!("- **Итог автопроверки:** {}\n\n", report.problem_hint));

    md.push_str("## Активные модули\n```text\n");
    md.push_str(&report.modules_summary);
    md.push_str("\n```\n\n");

    md.push_str("## Предупреждения и ошибки сессии\n");
    if warnings_errors.is_empty() {
        md.push_str("_Предупреждений и ошибок не зафиксировано._\n\n");
    } else {
        md.push_str("```log\n");
        for w in warnings_errors {
            md.push_str(&w);
            md.push('\n');
        }
        md.push_str("```\n\n");
    }

    md.push_str("## Полный лог сессии (Ring Buffer)\n```log\n");
    md.push_str(&full_log);
    md.push_str("```\n");
    md
}
