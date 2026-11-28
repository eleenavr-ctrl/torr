# ATLAS.md — Карта файлов и модулей

## UI (`ui/`)
- `ui/app.slint` — Главное адаптивное 3-панельное окно (`AppWindow`) и жесты свайпа.
- `ui/sidebar.slint` — Боковая навигационная панель Desktop/Tablet (`DesktopSidebar`) и карточка LAMPA LAN.
- `ui/overlays.slint` — Мобильная шапка/табы/drawer, `LightboxModal`, `SyncConflictModal`, `ToastBanner`.
- `ui/context_panel.slint` — Контекстная правая панель (`ContextSidePanel`: переключатель `DiagnosticsPanel` / `TorrentDetailPanel`).
- `ui/settings_view.slint` — Прокручиваемый контейнер настроек (`SettingsView`).
- `ui/detached_window.slint` — Отсоединяемое окно (`DetachedWindow`) с общим `AppState`.
- `ui/theme.slint` — `Win11Theme`, брейкпоинты Desktop/Tablet/Mobile и структуры данных.
- `ui/globals.slint` — `AppState` (единый источник телеметрии, счётчиков и моделей).
- `ui/components.slint` — Базовые компоненты Fluent (`FluentCard`, `FluentButton`, `ThemeSelector`, `FilterPill`, `StatTile`, `SvgIcon`).
- `ui/torrent_row.slint` — Компактная 2-строчная карточка раздачи (`DenseTorrentRow`).
- `ui/diagnostics_panel.slint` — Контекстная панель диагностики (4 карточки, консоль логов, экспорт для ИИ).
- `ui/settings_core_section.slint` — Настройки темы, языка, автозапуска, WebDAV/SecretStore, профилировщика и «О программе».
- `ui/workspace_builder.slint` — Конструктор пресетов и расположения блоков (`1/3`, `2/3`, `3/3`).
- `ui/status_bar.slint` — Нижняя панель статуса (`Portable (UserData/)`, синхронизация, версия).
- `ui/modules/torrserver/` — `torrents_view.slint`, `detail_panel.slint`, `detail_peers.slint`, `search_view.slint`, `history_view.slint`, `settings_section.slint`, `settings_rutracker.slint`.

## Rust (`src/`)
- `src/main.rs` — Входная точка Windows.
- `src/lib.rs` — Инициализация `AppPaths`, логирования, SingleInstance, SQLite, Config, SecretStore, Registry и окна.
- `src/ui_bridge.rs`, `src/ui_services.rs`, `src/ui_detached.rs` — Мосты к Slint UI (ядро, WebDAV/профилировщик/диагностика/конструктор, отсоединяемые окна).
- `src/core/` — Чистое переносимое ядро (`error`, `event_bus`, `paths`, `config`, `db`, `secrets`, `sync`, `storage`, `diagnostics`).
- `src/platform/` — Абстракции и Windows-реализация (`autostart`, `dpapi`, `metrics`, `single_instance`, `theme`, `tray`, `window_hooks`).
- `src/services/updater.rs` — Ручная проверка обновлений GitHub.
- `src/modules/torrserver/` — Логика TorrServer MatriX + `ui_adapter.rs`.
