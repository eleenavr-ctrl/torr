# ARCHITECTURE.md — Архитектура TorrServerUI

## 1. Концепция: «Ядро оболочки + подключаемые модули»

Приложение разделено на два независимых слоя:

```
┌───────────────────────────────────────────────────────────────────┐
│                     Slint UI (Fluent Design)                      │
│  ui/app.slint | ui/theme.slint | ui/components.slint              │
│  ui/modules/torrserver/{torrents,search,history,settings}.slint   │
└───────────────────────────────┬───────────────────────────────────┘
                                │ Bindings & Callbacks (Event Loop)
┌───────────────────────────────▼───────────────────────────────────┐
│                         Слой 1: Core                              │
│  src/main.rs          — Win32 Mutex, HWND/DWM, Drop, запуск       │
│  src/core/storage.rs  — Портативные пути ./data/, config.json     │
│  src/core/diagnostics.rs — Ring Buffer (300), отчёты, диагностика │
│  src/core/tray.rs     — Системный трей + Win32 GetMessageW        │
│  src/core/mod.rs      — Тема Win11, RGBA-иконка, автозапуск       │
└───────────────────────────────┬───────────────────────────────────┘
                                │ Трейт AppModule + ModuleRegistry
┌───────────────────────────────▼───────────────────────────────────┐
│                       Слой 2: Modules                             │
│  src/modules/mod.rs   — Контракт AppModule, сканер ./data/modules │
│  src/modules/torrserver/                                          │
│    ├── mod.rs            — Реализация AppModule, UI-поллинг       │
│    ├── server_manager.rs — Процесс TorrServer, stdout/stderr, OTA │
│    ├── api.rs            — HTTP API клиента (reqwest + rustls)    │
│    ├── player.rs         — Детект плееров, .m3u, history.json     │
│    └── clipboard.rs      — Перехват magnet-ссылок                 │
└───────────────────────────────────────────────────────────────────┘
```

## 2. Модель многопоточности

1. **Поток 1 — Slint Main Event Loop (`main.rs`):**
   - Владеет `AppWindow` и обновляет `ModelRc<T>` только через `slint::invoke_from_event_loop`.
   - При закрытии окна крестиком возвращает `CloseRequestResponse::HideWindow`, оповещая `ModuleRegistry::notify_visibility(false)`.
2. **Поток 2 — Системный трей (`core/tray.rs`):**
   - Запускает отдельный поток с собственным циклом Win32 (`GetMessageW` / `TranslateMessage` / `DispatchMessageW`).
   - Обработчики `TrayIconEvent` и `MenuEvent` пробрасывают команды в Slint UI через `slint::invoke_from_event_loop`.
   - При выходе получает `WM_QUIT` через `PostThreadMessageW`, корректно удаляя иконку из трея.
3. **Потоки Модуля TorrServer (`modules/torrserver/`):**
   - **Engine stdout/stderr reader threads:** читают вывод дочернего `TorrServer-windows-amd64.exe` и передают строки в `diagnostics`.
   - **UI Polling thread:** опрашивает `/echo` и `/torrents` каждые ~1.5 сек, но **полностью засыпает (0% CPU)**, когда окно скрыто в трей (`window_visible == false`).
   - **Clipboard watcher thread:** работает с интервалом ~1.2 сек только при активном переключателе и открытом/разрешённом режиме.
   - **Worker threads:** краткоживущие потоки для сетевых запросов (поиск, добавление торрента, обновление движка, диагностика).

## 3. Поток данных диагностики

Любой компонент вызывает `diagnostics::info/warn/error(tag, message)`:
1. Запись помещается в `Mutex<VecDeque<DiagnosticEvent>>` (лимит 300 записей).
2. При открытой вкладке Настроек (или по запросу «Проверить сейчас») UI получает актуальный срез логов и агрегированный отчёт `Core` + `ModuleRegistry::diagnostics_snapshot()`.
3. Кнопка «Скопировать для ИИ» формирует единый Markdown-документ с версиями, состоянием порта, статусом бинарника, снимком модулей и полным логом сессии.
