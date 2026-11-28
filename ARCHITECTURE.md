# ARCHITECTURE.md — Архитектура TorrServerUI

```
src/
├── main.rs                    # Тонкий бинарный вход (windows_subsystem = "windows")
├── lib.rs                     # Оркестрация запуска/завершения torrserver_ui::run()
├── ui_bridge.rs               # Привязка AppState, CoreSettings, WebDAV, Profiler, Workspace, Diagnostics
├── ui_detached.rs             # Управление DetachedWindow и проброс коллбэков
├── core/                      # Чистое ядро (без Slint и без Win32 вызовов)
│   ├── error.rs               # CoreError (thiserror)
│   ├── event_bus.rs           # EventBus + AppEvent
│   ├── paths.rs               # AppPaths (UserData/, --data-dir, read_only fallback, миграция data/)
│   ├── config.rs              # Атомарный settings.json, schema_version, unknown fields, merge
│   ├── db.rs                  # SQLite WAL, миграции user_version, VACUUM INTO бэкапы, RO ?immutable=1
│   ├── secrets.rs             # FileSecretStore (DPAPI + Argon2/ChaCha20Poly1305)
│   ├── sync.rs                # SyncProvider + WebDavSyncProvider
│   ├── storage.rs             # Совместимый фасад PortablePaths & TorrServerConfig для модулей
│   └── diagnostics.rs         # Журнал (Logs/ + RingBuffer), panic hook (crash.log), диагностика TorrServer
├── platform/                  # Платформенный слой
│   ├── mod.rs                 # Трейты Autostart, ProcessMetrics, SingleInstance, SystemTheme, TrayService
│   └── windows/               # Реализация Windows x64 (DPAPI, PDH/ProcessMetrics, Autostart, IPC, DWM, Tray, Hooks)
├── services/
│   └── updater.rs             # Ручная проверка обновлений GitHub Releases
└── modules/
    ├── mod.rs                 # AppModule & ModuleRegistry
    └── torrserver/            # Бэкенд TorrServer MatriX + ui_adapter.rs
```
