use crate::core::error::{CoreError, CoreResult};
use rusqlite::{params, Connection, OpenFlags};
use std::fs;
use std::path::{Path, PathBuf};

const CURRENT_DB_VERSION: i32 = 1;

pub struct AppDatabase {
    conn: Connection,
    read_only: bool,
    db_path: PathBuf,
}

impl AppDatabase {
    pub fn open(db_path: &Path, read_only: bool) -> CoreResult<Self> {
        if read_only {
            let mut ro_url = format!("file:{}?immutable=1", db_path.display());
            ro_url = ro_url.replace('\\', "/");
            let conn = Connection::open_with_flags(
                ro_url,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )?;
            return Ok(Self {
                conn,
                read_only: true,
                db_path: db_path.to_path_buf(),
            });
        }

        let conn = Connection::open(db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )?;

        let mut db = Self {
            conn,
            read_only: false,
            db_path: db_path.to_path_buf(),
        };
        db.migrate()?;
        Ok(db)
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn user_version(&self) -> CoreResult<i32> {
        let ver: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        Ok(ver)
    }

    fn migrate(&mut self) -> CoreResult<()> {
        let ver = self.user_version()?;
        if ver > CURRENT_DB_VERSION {
            return Err(CoreError::SchemaDowngrade {
                found: ver as u32,
                supported: CURRENT_DB_VERSION as u32,
            });
        }
        if ver < 1 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS kv_meta (
                    k TEXT PRIMARY KEY,
                    v TEXT NOT NULL,
                    updated_at INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE IF NOT EXISTS workspace_presets (
                    name TEXT PRIMARY KEY,
                    payload_json TEXT NOT NULL,
                    updated_at INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE IF NOT EXISTS torrents_notes (
                    hash TEXT PRIMARY KEY,
                    title TEXT NOT NULL DEFAULT '',
                    favorite INTEGER NOT NULL DEFAULT 0,
                    updated_at INTEGER NOT NULL DEFAULT 0
                 );
                 PRAGMA user_version = 1;",
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn set_meta(&self, key: &str, value: &str) -> CoreResult<()> {
        if self.read_only {
            return Err(CoreError::ReadOnly);
        }
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT INTO kv_meta (k, v, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(k) DO UPDATE SET v=excluded.v, updated_at=excluded.updated_at",
            params![key, value, now],
        )?;
        Ok(())
    }

    pub fn get_meta(&self, key: &str) -> CoreResult<Option<String>> {
        let mut stmt = self.conn.prepare("SELECT v FROM kv_meta WHERE k = ?1")?;
        let mut rows = stmt.query(params![key])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    pub fn snapshot_to(&self, backups_dir: &Path, target_name: &str) -> CoreResult<PathBuf> {
        if self.read_only {
            return Err(CoreError::ReadOnly);
        }
        fs::create_dir_all(backups_dir)?;
        let out_path = backups_dir.join(target_name);
        if out_path.exists() {
            let _ = fs::remove_file(&out_path);
        }
        let escaped = out_path.to_string_lossy().replace('\'', "''");
        self.conn.execute(&format!("VACUUM INTO '{escaped}'"), [])?;
        rotate_backups(backups_dir, 3);
        Ok(out_path)
    }

    pub fn checkpoint_and_close(self) {
        if !self.read_only {
            let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        }
        let _ = self.conn.close();
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }
}

fn rotate_backups(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(PathBuf, std::time::SystemTime)> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("db") {
                let mtime = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                Some((p, mtime))
            } else {
                None
            }
        })
        .collect();
    files.sort_by(|a, b| b.1.cmp(&a.1));
    for (old, _) in files.into_iter().skip(keep) {
        let _ = fs::remove_file(old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_wal_migrates_and_creates_snapshot() {
        let base = std::env::temp_dir().join(format!("ts_db_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let db_path = base.join("app_data.db");
        let backups = base.join("Backups");

        let db = AppDatabase::open(&db_path, false).unwrap();
        assert_eq!(db.user_version().unwrap(), 1);
        db.set_meta("last_check", "ok").unwrap();
        assert_eq!(db.get_meta("last_check").unwrap().as_deref(), Some("ok"));
        let snap = db.snapshot_to(&backups, "snap1.db").unwrap();
        assert!(snap.exists());
        db.checkpoint_and_close();

        let ro = AppDatabase::open(&db_path, true).unwrap();
        assert!(ro.is_read_only());
        assert_eq!(ro.get_meta("last_check").unwrap().as_deref(), Some("ok"));
        assert!(ro.set_meta("x", "y").is_err());

        let _ = fs::remove_dir_all(&base);
    }
}
