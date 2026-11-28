# AGENTS.md — Правила для AI-инженеров

1. **Границы слоёв:**
   - `src/core/` не импортирует `slint` или платформенные Win32 WinAPI модули.
   - `src/platform/` изолирует все Win32 / OS вызовы за трейтами `platform/mod.rs`.
   - Бизнес-логику TorrServer (`src/modules/torrserver/{api,server_manager,player,metadata,rutracker,rutracker_webview,clipboard}.rs`) изменять только точечно; UI-связки выносить в `ui_adapter.rs`.
2. **Размер модулей:** Новые/переписываемые `.rs` и `.slint` файлы — не более ~500 строк.
3. **Память проекта:** После архитектурных изменений обновлять `ATLAS.md`, `DECISIONS.md`, `HANDOFF.yaml`, `CHANGELOG.md`.
4. **Безопасность:** Не логировать пароли, cookie, WebDAV-токены и ключи API в диагностику/логи. Использовать `SecretStore`.
