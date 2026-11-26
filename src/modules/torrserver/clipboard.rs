use crate::core::diagnostics;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub struct ClipboardWatcher {
    enabled: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl ClipboardWatcher {
    pub fn start<F>(initial_enabled: bool, on_magnet: F) -> Self
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        let enabled = Arc::new(AtomicBool::new(initial_enabled));
        let paused = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));

        let e_flag = Arc::clone(&enabled);
        let p_flag = Arc::clone(&paused);
        let s_flag = Arc::clone(&stop);

        thread::Builder::new()
            .name("ts-clipboard-watcher".to_string())
            .spawn(move || {
                let mut last_seen = String::new();
                while !s_flag.load(Ordering::Relaxed) {
                    if e_flag.load(Ordering::Relaxed) && !p_flag.load(Ordering::Relaxed) {
                        if let Ok(mut cb) = arboard::Clipboard::new() {
                            if let Ok(text) = cb.get_text() {
                                let trimmed = text.trim();
                                if trimmed.starts_with("magnet:?xt=") && trimmed != last_seen {
                                    last_seen = trimmed.to_string();
                                    diagnostics::info(
                                        "[TS:API]",
                                        "Перехвачена Magnet-ссылка из буфера обмена",
                                    );
                                    on_magnet(trimmed.to_string());
                                }
                            }
                        }
                    }
                    thread::sleep(Duration::from_millis(1200));
                }
            })
            .ok();

        Self {
            enabled,
            paused,
            stop,
        }
    }

    pub fn set_enabled(&self, val: bool) {
        self.enabled.store(val, Ordering::Relaxed);
    }

    pub fn set_paused(&self, val: bool) {
        self.paused.store(val, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for ClipboardWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}
