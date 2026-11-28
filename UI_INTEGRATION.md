# UI_INTEGRATION.md — Справочник привязки Slint UI ↔ Rust

## Глобальные состояния (`ui/app.slint` экспортирует в Rust)
- `Win11Theme`: `theme-mode` (`0=System`, `1=Light`, `2=Dark`), `system-is-dark`, `viewport-width` (автопривязка к `AppWindow.width`).
- `AppState`: `active-tab`, `detail-tab`, `telemetry-*`, `filter-mode`, `count-*`, `torrents`, `selected-files`, `search-results`, `history-items`, `diag-logs`, `registered-modules`, `detail-peers`, `detail-speed-history`, `workspace-blocks`, `workspace-presets`, `data-dir-path`, `data-read-only`, `sync-status-text`, `app-lang`.

## Основные окна
- `AppWindow::new()` — главное 3-панельное адаптивное окно (Desktop/Tablet/Mobile).
- `DetachedWindow::new()` — отсоединяемое окно модуля с пробросом всех действий на `AppWindow`.

## Новые коллбэки `AppWindow`
- Фильтры и медиа: `set-torrents-filter(int)`, `watch-trailer(string, string)`.
- Ядро и сервисы: `change-language(int)`, `set-autostart-mode(int)`, `repair-autostart()`, `toggle-close-to-tray(bool)`, `save-webdav-settings()`, `test-webdav-connection()`, `sync-webdav-now()`, `resolve-sync-conflict(int)`, `toggle-profiler(bool)`, `export-profiler-report()`, `check-github-updates()`, `detach-module(int)`.
- Конструктор: `ws-load-preset(int)`, `ws-save-preset(string)`, `ws-delete-preset(int)`, `ws-set-block-span(string, int)`, `ws-move-block(string, int)`.
