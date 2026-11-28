# SPEC.md — Функциональная спецификация v0.2.0

## UI и навигация
- Адаптивные брейкпоинты `Win11Theme`:
  - Desktop (`>= 1024px`): 3 панели (Sidebar `184px` + Workspace + Context `320px`).
  - Tablet (`680–1023px`): Icon Rail `56px` + Workspace + Context slide-over.
  - Mobile (`< 680px`): Bottom TabBar + Left Slide-in Drawer + Right Context slide-over + жесты свайпа (`|dx| > 45px`, отступ `24px` от края).
- `TorrentDetailPanel`: вкладки **Обзор** (постер, КП/IMDb, кнопка «Трейлер», сворачиваемый сюжет), **Файлы** (`28px` строки с тегом кодека и отметкой просмотра), **Кадры** (сетка + Lightbox), **Пиры** (мини-график скорости + пиры). В режиме Настроек правая панель показывает `DiagnosticsPanel`.
- Внешние окна (`DetachedWindow`): вынос Раздач / Поиска / Истории / Конструктора с общим `AppState`.

## Хранилище и надёжность
- `AppPaths`: `UserData/` рядом с `.exe` (или `--data-dir`), `Exports/`, `Backups/` (до 3 копий), `Logs/` (ротация `3x1MB` + `crash.log`), режим `read_only` при запрете записи.
- `settings.json`: `schema_version = 2`, `updated_at`, `device_id`, `extra` flatten, атомарная запись (`tmp` + `sync_all` + `rename`), защита от downgrade.
- `app_data.db`: SQLite WAL + `foreign_keys=ON` + `busy_timeout=5000` + миграции `user_version` + `VACUUM INTO` снапшоты + `wal_checkpoint(TRUNCATE)` при выходе.
- `SecretStore`: Windows DPAPI (`CryptProtectData`/`CryptUnprotectData`) или Мастер-пароль (`argon2` + `chacha20poly1305`).
- `WebDavSyncProvider`: lock-маркер `.sync.lock`, слияние пресетов и настроек, проверка конфликта БД и выбор пользователя (`KeepLocal` / `UseRemote`).
