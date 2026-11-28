# BRIEF — Паспорт проекта TorrServerUI

- **Название:** `TorrServerUI` (crate: `torrserver-ui`, бинарник: `torrserver-ui.exe` / артефакт `TorrServerUI.exe`)
- **Тип:** Портативное десктопное Windows-first приложение (Rust + Slint UI Fluent) с чистым слоистым ядром, готовым к переносу на Linux, macOS и Android.
- **Лицензия GUI:** Slint **Royalty-Free** с обязательным бейджем «Made with Slint» в разделе «О программе».
- **Ключевые требования:**
  1. Полное сохранение существующего бэкенда TorrServer MatriX (`api.rs`, `server_manager.rs`, `player.rs`, `metadata.rs`, `rutracker.rs`, `rutracker_webview.rs`, `clipboard.rs`).
  2. Windows 11 Fluent адаптивный 3-панельный UI (Desktop / Tablet / Mobile + свайпы + Lightbox + Lampa LAN + 3 темы + RU/EN локализация + SVG иконки + Конструктор рабочего пространства + Отсоединяемые окна).
  3. Портативная структура `UserData/` рядом с `.exe` (`--data-dir`, автомиграция `data/`, read-only режим), атомарный `settings.json`, SQLite `WAL`, DPAPI/Argon2+ChaCha20 `SecretStore`, `WebDavSyncProvider`, Single Instance + `interprocess`, Dual Autostart с починкой пути, Профилировщик процесса, Diagnostics + crash.log.
  4. Сборка Windows x64 через GitHub Actions (`.exe` + Portable `.zip` + `SHA256SUMS.txt` + `Cargo.lock`).
