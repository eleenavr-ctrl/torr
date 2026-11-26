# STATUS.md — Статус подсистем TorrServerUI

## Версия `v0.1.0`

| Подсистема | Статус | Примечание |
| --- | --- | --- |
| Ядро: портативное хранилище (`./data/`) | Готово | `config.json`, автосоздание папок |
| Ядро: тема Win11 Light/Dark + DWM | Готово | Системная / Светлая / Тёмная + titlebar |
| Ядро: системный трей и Single-Instance | Готово | Выделенный Win32-поток, пробуждение через `PostMessageW` |
| Ядро: диагностика в реальном времени | Готово | Ring buffer 300, проверка порта/пинга, экспорт для ИИ |
| Модуль №1: `ModuleRegistry` и контракт `AppModule` | Готово | Плюс сканирование `./data/modules/` |
| Модуль №1: управление `TorrServer.exe` + OTA | Готово | `CREATE_NO_WINDOW`, `/shutdown`, GitHub Releases |
| Модуль №1: API клиент + поиск RuTor/JacRed | Готово | `reqwest` с `rustls-tls` |
| Модуль №1: плееры, `.m3u`, `history.json` | Готово | PotPlayer/MPV/VLC/MPC-BE/HC + ручной `.exe` |
| Модуль №1: буфер обмена + Drag-and-Drop | Готово | `WM_DROPFILES` + фоновый перехват Magnet |
| UI: Slint `fluent` (4 вкладки + статус-бар + ПКМ) | Готово | `PopupWindow`, `SettingsExpander`, 0% CPU в трее |
| Сборка CI (GitHub Actions `windows-latest`) | Готово | `.github/workflows/build.yml` |

## Бэклог будущих версий
- **v0.2.0:** Интерактивный протокол IPC (`stdin/stdout` JSON-RPC) для внешних плагинов из `./data/modules/*.exe`.
- **v0.2.0:** Предпросмотр постеров и расширенное описание раздач (TMDB / Kinopoisk metadata опционально).
- **v0.3.0:** Модуль №2 (например, встроенный планировщик загрузок или локальный DLNA/media-инспектор).
