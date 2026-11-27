# ATLAS.md — Карта репозитория и связей

## 1. Структура файлов (32 файла)

| Путь | Назначение |
| --- | --- |
| `Cargo.toml` | Манифест пакета, зависимости (Slint 1.9, reqwest+rustls, wry 0.50, tray-icon, windows-sys 0.59) |
| `build.rs` | Компиляция `ui/app.slint` со стилем `fluent` |
| `.github/workflows/build.yml` | CI/CD сборка `x86_64-pc-windows-msvc` -> `dist/TorrServerUI.exe` |
| `AGENT.md` | Регламент разработки для ИИ-агентов и чек-лист добавления модулей |
| `ARCHITECTURE.md` | Описание двухслойной архитектуры, потоков и шины диагностики |
| `ATLAS.md` | Карта файлов, связей Slint ↔ Rust и навигатор правок |
| `DECISIONS.md` | Журнал архитектурных решений (ADR-001..ADR-006) |
| `BUGS.md` | Реестр нюансов Win32/Slint/TorrServer и исправленных проблем |
| `CHANGELOG.md` | История изменений проекта |
| `STATUS.md` | Статус готовности подсистем `v0.1.0` и бэклог |
| `HANDOFF.md` | Инструкция передачи контекста, сборка в GitHub Actions, чек-лист запуска |
| `README.md` | Документация пользователя и структура `./data/` |
| `ui/theme.slint` | Глобальная тема Windows 11 (Light/Dark), палитра и структуры моделей |
| `ui/components.slint` | Fluent-компоненты: `FluentCard`, `FluentButton`, `FluentNavItem`, `SettingsExpander` |
| `ui/modules/torrserver/torrents_view.slint` | Экран «Раздачи», панель файлов, фильтры, контекстные меню ПКМ |
| `ui/modules/torrserver/search_view.slint` | Экран «Поиск» по трекерам с фильтрами качества и сортировкой |
| `ui/modules/torrserver/history_view.slint` | Экран «История» с избранным и отметками просмотра |
| `ui/modules/torrserver/settings_section.slint` | Секции настроек модуля TorrServer (сервер, кеш, плееры) |
| `ui/app.slint` | Главное окно `AppWindow`, `NavigationView`, вкладка «Настройки», статус-бар |
| `src/main.rs` | Точка входа, Single-Instance Mutex, Win32 subclassing (DWM/Drop/Wake), цикл UI |
| `src/core/mod.rs` | Тема Win11, DWM Dark Mode, программная RGBA-иконка, автозапуск `.lnk` |
| `src/core/storage.rs` | Портативные пути `./data/`, сериализация `config.json` |
| `src/core/diagnostics.rs` | Ring-буфер на 300 событий, проверка порта/версий, экспорт отчёта и Markdown для ИИ |
| `src/core/tray.rs` | Системный трей (`tray-icon`), меню ПКМ, поток Win32 `GetMessageW` |
| `src/modules/mod.rs` | Контракт `AppModule`, `ModuleInfo`, `ModuleStatus`, `ModuleRegistry`, скан `./data/modules/` |
| `src/modules/torrserver/mod.rs` | Модуль TorrServer, связывание UI, фоновый поллинг с паузой в трее |
| `src/modules/torrserver/api.rs` | HTTP-клиент TorrServer (`/echo`, `/torrents`, `/torrent/upload`, `/settings`, `/search` + JacRed) |
| `src/modules/torrserver/server_manager.rs` | Запуск/остановка `TorrServer-windows-amd64.exe` (`CREATE_NO_WINDOW`), автообновление |
| `src/modules/torrserver/player.rs` | Автопоиск плееров, запуск потока, генерация `.m3u`, `history.json` |
| `src/modules/torrserver/rutracker.rs` | Клиент RuTracker (`windows-1251`, авторизация, прокси, парсинг `tracker.php`, загрузка `.torrent`) |
| `src/modules/torrserver/rutracker_webview.rs` | Ленивый встроенный браузер Edge WebView2 (`wry`) для обхода Cloudflare и скачивания `.torrent` с RuTracker |
| `src/modules/torrserver/clipboard.rs` | Фоновый перехват `magnet:?xt=...` из буфера обмена |

## 2. Таблица связей `AppWindow` (Slint ↔ Rust)

| Группа в `ui/app.slint` | Свойства (`in-out`) | Колбэки | Обработчик в Rust |
| --- | --- | --- | --- |
| Навигация и тема | `active-tab`, `theme-mode`, `autostart-enabled`, `start-minimized`, `keep-in-memory`, `app-icon` | `change-theme-mode`, `toggle-autostart`, `toggle-start-minimized`, `toggle-keep-in-memory`, `open-diagnostics-tab` | `src/main.rs`, `src/core/mod.rs` |
| Статус-бар | `status-healthy`, `status-text`, `status-version`, `status-speed`, `status-active-count` | `open-diagnostics-tab` | `src/modules/torrserver/mod.rs` |
| Раздачи | `torrents`, `torrents-sort-mode`, `selected-files`, `magnet-input`, `only-video-files`, `engine-missing`, `engine-update-progress`, `engine-version-presets`, `engine-version-selected-idx`, `status-healthy` | `add-magnet`, `paste-and-add-magnet`, `pick-torrent-file`, `select-torrent`, `set-torrents-sort`, `play-file`, `play-all-m3u`, `drop-torrent`, `remove-torrent`, `select-engine-version`, `refresh-engine-versions`, `download-update-engine`, `pick-engine-exe`, `export-torrents`, `import-torrents` | `src/modules/torrserver/mod.rs` |
| Поиск | `search-query`, `search-quality-filter`, `search-sort-mode`, `search-source-mode`, `search-results` | `search-torrents`, `fetch-top-24h`, `set-search-quality`, `set-search-sort`, `set-search-source`, `add-search-result`, `play-search-result` | `src/modules/torrserver/mod.rs` |
| История | `history-items`, `history-only-favorites` | `toggle-history-favorites`, `play-history-item`, `toggle-history-favorite`, `remove-history-item` | `src/modules/torrserver/mod.rs` |
| Настройки TS | `ts-mode`, `ts-port`, `ts-remote-url`, `ts-cache-size-mb`, `engine-update-progress`, `engine-version-presets`, `engine-version-selected-idx`, `engine-watchdog`, `keep-engine-on-exit`, `player-presets`, `clipboard-watch`, `rt-*` (`rt-use-webview`) | `save-ts-connection-settings`, `restart-ts-engine`, `select-engine-version`, `refresh-engine-versions`, `download-update-engine`, `pick-engine-exe`, `save-ts-cache-settings`, `detect-players`, `pick-player-exe`, `save-player-settings`, `save-and-test-rutracker`, `open-rutracker-webview` | `src/modules/torrserver/mod.rs` |
| Диагностика и модули | `registered-modules`, `diag-logs`, `diag-problem-hint`, `diag-port-status`, `diag-api-ping` | `run-diagnostics-now`, `copy-diagnostics-report`, `copy-diagnostics-for-ai`, `clear-diagnostics-logs` | `src/main.rs`, `src/core/diagnostics.rs` |

## 3. Шпаргалка «В какой файл вносить изменения»
- **Изменить цвета, скругления или шрифты UI:** `ui/theme.slint` и `ui/components.slint`.
- **Добавить кнопку или колонку в список раздач / файлов:** `ui/modules/torrserver/torrents_view.slint` + `src/modules/torrserver/mod.rs`.
- **Изменить параметры запуска `TorrServer.exe` или логику GitHub-обновления:** `src/modules/torrserver/server_manager.rs`.
- **Добавить новый трекер или эндпоинт TorrServer API:** `src/modules/torrserver/api.rs`.
- **Добавить поддержку нового видеоплеера:** `src/modules/torrserver/player.rs`.
- **Изменить состав системного трея:** `src/core/tray.rs`.
- **Добавить новый диагностический чек или формат отчёта:** `src/core/diagnostics.rs`.
- **Подключить новый модуль (Модуль №2):** `src/modules/mod.rs`, новая папка `src/modules/<id>/`, `ui/app.slint`.
