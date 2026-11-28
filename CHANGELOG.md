# Changelog — TorrServerUI

## [0.2.0] - 2026-11-28
### Добавлено
- **Трёхуровневый адаптивный Windows 11 Fluent UI (`Win11Theme`, `AppState`)**:
  - Desktop (`>=1024px`, 3 панели), Tablet (`680–1024px`, компактный rail `56px` + выездная контекстная панель), Mobile (`<680px`, нижний TabBar + свайп-меню).
  - 3 режима темы: Адаптивная системная, Светлая Mica (`#F3F3F3`/`#FFFFFF`), Тёмная Mica (`#202020`/`#2B2B2B`) + DWM тёмная рамка окна.
  - Сверхплотный список раздач (`DenseTorrentRow`, 2 строки, 3px прогресс-бар, теги качества/трекера, фильтры со счётчиками) и строка живой телеметрии.
  - Панель деталей (`TorrentDetailPanel`) с 4 вкладками (*Обзор + Трейлер*, *Файлы + Кодеки*, *Кадры + Lightbox*, *Пиры + График скорости*), а в Настройках — `DiagnosticsPanel`.
  - Конструктор рабочего пространства (`WorkspaceBuilderView`) с пресетами и отсоединяемыми окнами (`DetachedWindow`).
- **Портативное ядро и хранилище (`UserData/`)**:
  - Автономная папка `UserData/` рядом с `.exe` (поддержка `--data-dir`, автомиграция `./data/` -> `UserData/`, безопасный read-only fallback).
  - Атомарный `settings.json` (`schema_version`, защита от downgrade, сохранение неизвестных полей), SQLite `app_data.db` (`WAL`, `VACUUM INTO` бэкапы в `Backups/`), ротация логов в `Logs/` и `crash.log`.
  - Безопасное хранение секретов (`FileSecretStore`: Windows DPAPI и Argon2+ChaCha20Poly1305) и синхронизация WebDAV (Яндекс/Mail.ru/свой сервер).
  - Двойной автозапуск (ярлык Startup или `HKCU\...\Run` с `--minimized` и автопочинкой пути), Single Instance через Named Mutex + `interprocess`, встроенный профилировщик процесса, `ru`/`en` переводы и лицензия **Slint Royalty-Free («Made with Slint»)**.

## [0.1.0] - 2026-11-27
### Добавлено
- Базовый портативный клиент и лаунчер TorrServer MatriX на Rust + Slint с поддержкой RuTor / RuTracker (HTTP + WebView2), видеоплееров, Lampa LAN-адресов и диагностики.
