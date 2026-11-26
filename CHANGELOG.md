# CHANGELOG.md — История изменений

Все значимые изменения проекта документируются в этом файле.

## [Unreleased]
- Исправлены ошибки сборки под `x86_64-pc-windows-msvc`: включены фичи `raw-window-handle-06` для `slint` и `Win32_Security` для `windows-sys`, добавлен импорт `slint::ComponentHandle` в `src/core/mod.rs`.
- `diagnostics.rs` и `torrents_view.slint`: если сторонний/уже запущенный TorrServer отвечает на порту `8090`, отсутствие `data/torrserver/TorrServer-windows-amd64.exe` больше не помечается как критическая ошибка и не показывает блокирующий баннер на вкладке «Раздачи».
- `src/modules/torrserver/api.rs`: переработан разбор ответов `/search/?query=` и JacRed на базе `serde_json::Value` (поддержка полей `Link`/`Magnet`/`Hash`, `Title`/`Name`, `null`-значений), добавлена проверка `EnableRutorSearch` и резервные зеркала JacRed (`jacred.xyz`, `jacred.stream`, `jr.maxvol.pro`).
- `src/modules/torrserver/player.rs`: расширен автопоиск видеоплееров (`LocalAppData`, `AppData`, `Scoop`, `Chocolatey`, 64-битный K-Lite и read-only чтение `App Paths` из реестра Windows с раскрытием `%VAR%`).
- Экспорт и импорт списка раздач в `.json` (`⬇ Экспорт .json` / `⬆ Импорт .json` на вкладке «Раздачи»): перенос раздач с сохранением `hash`, `title`, `poster` и `data` между запущенными экземплярами TorrServer в один клик.
- Прямой поиск по **RuTracker** (`src/modules/torrserver/rutracker.rs`): авторизация логином/паролем (или cookie `bb_session`), кодировка `windows-1251`, настройка зеркала и прокси (`http`/`socks5h`), переключатель источника в «Поиске» (`Все` / `RuTor` / `RuTracker` / `JacRed`) и автоматическая загрузка `.torrent`/magnet в TorrServer.
- Подготовка к расширению модульной системы (поддержка внешнего каталога `./data/modules/`).

## [0.1.0] — 2026-11-26
### Добавлено
- **Ядро приложения (`src/core/`):**
  - Двухслойная архитектура `Core` + `ModuleRegistry` с контрактом `AppModule`.
  - Портативное хранение данных относительно `.exe` в `./data/config.json`, `./data/torrserver/`, `./data/modules/`.
  - Светлая и тёмная темы Windows 11 (Mica-палитра), синхронизация заголовка окна через `DwmSetWindowAttribute` (`DWMWA_USE_IMMERSIVE_DARK_MODE`) и `Palette.color-scheme`.
  - Программная генерация 32x32 RGBA-иконки приложения и системного трея без внешних файлов.
  - Системный трей (`tray-icon` + `muda`) в выделенном Win32-потоке с меню ПКМ и сворачиванием при закрытии.
  - Защита от повторного запуска через именованный Windows Mutex с разворачиванием существующего окна из трея.
  - Шина диагностики в реальном времени (кольцевой буфер на 300 событий, проверка порта/бинарника/ping API, экспорт отчёта и Markdown-снимка для ИИ).
- **Модуль №1 `torrserver` (`src/modules/torrserver/`):**
  - Локальный скрытый запуск `TorrServer-windows-amd64.exe` (`CREATE_NO_WINDOW`), перехват `stdout`/`stderr`, штатная остановка через `/shutdown` + `kill()`.
  - Удалённое подключение с поддержкой HTTP Basic Auth.
  - Автообновлятор движка с GitHub Releases (`YouROK/TorrServer`) с обходом блокировки файла на Windows.
  - Клиент TorrServer API (`/echo`, `/torrents`, `/torrent/upload`, `/settings`, `/search` + fallback на публичный API JacRed).
  - Автопоиск видеоплееров (PotPlayer, MPV, VLC, MPC-BE/HC), ручной выбор `.exe`, генерация плейлистов `.m3u`, история просмотров `history.json` с избранным и отметкой серий.
  - Опциональный перехват Magnet-ссылок из буфера обмена и Drag-and-Drop `.torrent`-файлов в окно.
- **UI и CI:**
  - Интерфейс Slint 1.9 (`fluent`) с боковой панелью `NavigationView`, вкладками *Раздачи*, *Поиск*, *История*, *Настройки* и контекстными меню `PopupWindow`.
  - GitHub Actions workflow `.github/workflows/build.yml` для сборки `x86_64-pc-windows-msvc`.
