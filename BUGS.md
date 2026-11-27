# BUGS.md — Известные технические нюансы и исправления

## 1. Зависание `TorrServer-windows-amd64.exe` и нештатное завершение (`POST /shutdown` → `GET /shutdown`)
- **Причина:** При обычном завершении родительского GUI-процесса консольный процесс TorrServer может остаться в памяти, а ранее использовавшийся `POST /shutdown` возвращал в MatriX `404` и приводил к принудительному `child.kill()` (`TerminateProcess`) через 800 мс без корректного закрытия `config.db` (`bbolt`).
- **Решение:** В `TorrServerApi::shutdown_server()` отправляется `GET /shutdown` (с фолбэком на `POST` при `404`/`405`) с таймаутом 1.5 с, а `ServerManager::stop()` ожидает штатного выхода до 3 секунд перед `kill()`. Дополнительно все добавленные задачи дублируются в `./data/torrserver/torrents_saved.json` (и `./data/torrserver/torrents/*.torrent`) с автовосстановлением на сервер при старте/перезапуске.

## 2. Блокировка `.exe` при автообновлении движка на Windows
- **Причина:** Windows не позволяет перезаписать исполняемый файл запущенного процесса.
- **Решение:** Скачивание новой версии выполняется во временный файл `TorrServer-windows-amd64.exe.new`, затем активный процесс полностью останавливается (`stop()`), старый файл заменяется (`fs::rename` / `fs::copy` + `remove_file`), после чего движок запускается заново.

## 3. Мерцание консольного окна при запуске TorrServer / утилит
- **Причина:** Стандартный `std::process::Command` на Windows открывает чёрное окно `conhost.exe`.
- **Решение:** Используется `std::os::windows::process::CommandExt::creation_flags(0x08000000)` (`CREATE_NO_WINDOW`), а `stdout`/`stderr` перенаправляются через `Stdio::piped()` в шину диагностики.

## 4. `windows-sys` 0.59: типы `HWND` и `HANDLE`
- **Нюанс:** Начиная с `windows-sys` 0.52, `HWND` и `HANDLE` являются `*mut c_void`, а не `isize`.
- **Решение:** Все нулевые указатели передаются как `std::ptr::null_mut()`, а приведение `NonZeroIsize` из `raw-window-handle` выполняется через `as HWND`.

## 5. `@children` внутри условного `if` в Slint
- **Нюанс:** Компилятор Slint запрещает размещать `@children` внутри блока `if`.
- **Решение:** В `SettingsExpander` (`ui/components.slint`) контейнер `@children` остаётся безусловным, а сворачивание реализовано через `height: root.expanded ? body-layout.preferred-height : 0px; clip: true; visible: root.expanded;`.

## 6. Порядок завершения трея при выходе
- **Нюанс:** Иконка в трее может «зависнуть» до наведения мыши, если поток трея убит аварийно.
- **Решение:** Поток трея сохраняет свой Win32 `ThreadId`; при выходе отправляется `PostThreadMessageW(tid, WM_QUIT, 0, 0)` и выполняется `join()`, что штатно удаляет иконку трея.

## 7. Нулевой список плееров (`DetectedPlayers: 0`) и ложные `[WARN]` от Go-движка
- **Нюанс:** При установке плееров в нестандартные пути/реестр или наличии только штатных средств Windows поиск по фиксированным путям возвращал `0`, а Go-пакет `log` в `TorrServer MatriX` выводит все обычные события (`New torrent`, `Create cache`, `Close cache`) в `stderr`.
- **Решение:** `player.rs` дополнен чтением веток `Uninstall` и файловых ассоциаций Windows + фолбэком на `wmplayer.exe` и `.m3u` системного обработчика; `server_manager.rs` фильтрует `stderr` движка по ключевым словам ошибки/предупреждения вместо безусловного `[WARN]`.
