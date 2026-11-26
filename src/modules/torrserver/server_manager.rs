use crate::core::diagnostics;
use crate::core::storage::{PortablePaths, TorrServerConfig};
use crate::modules::torrserver::api::TorrServerApi;
use reqwest::blocking::Client;
use serde::Deserialize;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
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

    pub fn start(&self, cfg: &TorrServerConfig) -> Result<(), String> {
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

        let port_str = cfg.port.to_string();
        let cache_str = self.paths.ts_cache_dir.to_string_lossy().to_string();

        let mut cmd = Command::new(&self.paths.torrserver_exe);
        cmd.args(["--port", &port_str, "--path", &cache_str, "--rdb"])
            .current_dir(&self.paths.torrserver_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

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
                        if !trimmed.is_empty() {
                            diagnostics::warn("[TS:ENGINE]", trimmed.to_string());
                        }
                    }
                })
                .ok();
        }

        if let Ok(mut guard) = self.child.lock() {
            *guard = Some(child);
        }

        diagnostics::info(
            "[TS:ENGINE]",
            format!("Локальный движок запущен на порту {}", cfg.port),
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
                for _ in 0..8 {
                    if let Ok(Some(_)) = child.try_wait() {
                        diagnostics::info("[TS:ENGINE]", "Движок штатно остановлен");
                        return;
                    }
                    thread::sleep(Duration::from_millis(80));
                }
                let _ = child.kill();
                let _ = child.wait();
                diagnostics::warn("[TS:ENGINE]", "Движок принудительно завершён (kill)");
            }
        }
    }

    pub fn restart(&self, cfg: &TorrServerConfig) -> Result<(), String> {
        diagnostics::info("[TS:ENGINE]", "Перезапуск TorrServer...");
        self.stop(cfg);
        thread::sleep(Duration::from_millis(250));
        self.start(cfg)
    }

    pub fn update_from_github<F>(
        &self,
        cfg: &TorrServerConfig,
        on_status: F,
    ) -> Result<String, String>
    where
        F: Fn(&str),
    {
        on_status("Запрос последнего релиза YouROK/TorrServer...");
        diagnostics::info("[TS:UPDATER]", "Проверка релизов YouROK/TorrServer на GitHub");

        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| format!("Ошибка HTTP-клиента: {e}"))?;

        let release = client
            .get("https://api.github.com/repos/YouROK/TorrServer/releases/latest")
            .header("User-Agent", "TorrServerUI/0.1.0")
            .send()
            .map_err(|e| format!("Не удалось подключиться к GitHub API: {e}"))?
            .error_for_status()
            .map_err(|e| format!("Ошибка ответа GitHub API: {e}"))?
            .json::<GitHubRelease>()
            .map_err(|e| format!("Ошибка разбора JSON релиза: {e}"))?;

        let asset = release
            .assets
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case("TorrServer-windows-amd64.exe"))
            .ok_or_else(|| {
                "В последнем релизе не найден файл TorrServer-windows-amd64.exe".to_string()
            })?;

        on_status(&format!(
            "Скачивание {} ({})...",
            asset.name, release.tag_name
        ));
        diagnostics::info(
            "[TS:UPDATER]",
            format!("Скачивание {} ({})", release.tag_name, asset.browser_download_url),
        );

        let resp = client
            .get(&asset.browser_download_url)
            .header("User-Agent", "TorrServerUI/0.1.0")
            .send()
            .map_err(|e| format!("Ошибка скачивания бинарника: {e}"))?
            .error_for_status()
            .map_err(|e| format!("Ошибка HTTP при скачивании бинарника: {e}"))?;

        let new_path = self.paths.torrserver_exe.with_extension("exe.new");
        let bytes = resp
            .bytes()
            .map_err(|e| format!("Ошибка чтения потока файла: {e}"))?;

        {
            let mut file = fs::File::create(&new_path)
                .map_err(|e| format!("Не удалось создать временный файл .new: {e}"))?;
            file.write_all(&bytes)
                .map_err(|e| format!("Ошибка записи .new файла: {e}"))?;
        }

        on_status("Остановка текущего процесса и замена файла...");
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
            format!("Движок обновлён до версии {}", release.tag_name),
        );

        if cfg.mode == 0 {
            on_status("Запуск обновлённого TorrServer...");
            self.start(cfg)?;
        }

        Ok(release.tag_name)
    }
}
