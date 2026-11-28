use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("Ошибка ввода-вывода: {0}")]
    Io(#[from] std::io::Error),

    #[error("Ошибка JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Ошибка базы данных SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Ошибка безопасности / шифрования: {0}")]
    Crypto(String),

    #[error("Ошибка синхронизации WebDAV: {0}")]
    Sync(String),

    #[error("Режим только для чтения: запись запрещена")]
    ReadOnly,

    #[error("Версия конфигурации новее поддерживаемой ({found} > {supported})")]
    SchemaDowngrade { found: u32, supported: u32 },

    #[error("{0}")]
    Other(String),
}

pub type CoreResult<T> = Result<T, CoreError>;
