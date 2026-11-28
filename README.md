# TorrServerUI — Портативный клиент TorrServer MatriX (Windows 11 Fluent)

Портативное десктопное приложение на **Rust + Slint (Fluent)** с адаптивным интерфейсом Windows 11, встроенным управлением движком TorrServer MatriX, поиском по трекерам, плеером, медиа-карточками, WebDAV-синхронизацией и поддержкой Lampa LAN.

## Портативный режим (`UserData/`)
Все данные (`settings.json`, `app_data.db`, `secrets.enc`, `Logs/`, `Backups/`, `Exports/`, `torrserver/`) хранятся в папке `UserData/` рядом с `TorrServerUI.exe` (переопределение через `--data-dir <путь>`). При наличии старой папки `./data/` выполняется автоматический перенос.

## Сборка (Windows x64)
Сборка и проверка запускаются автоматически через GitHub Actions (`.github/workflows/build.yml`):
```bash
cargo check
cargo test --lib
RUSTFLAGS="-C target-feature=+crt-static" cargo build --release --target x86_64-pc-windows-msvc
```

## Лицензия
Код проекта распространяется по лицензии **MIT**. Графический интерфейс использует библиотеку Slint по лицензии **Slint Royalty-Free** («Made with Slint»).
