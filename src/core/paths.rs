use crate::core::error::CoreResult;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub exe_path: PathBuf,
    pub exe_dir: PathBuf,
    pub user_data_dir: PathBuf,
    pub read_only: bool,
    pub settings_path: PathBuf,
    pub db_path: PathBuf,
    pub secrets_path: PathBuf,
    pub logs_dir: PathBuf,
    pub crash_log_path: PathBuf,
    pub exports_dir: PathBuf,
    pub backups_dir: PathBuf,
    pub modules_dir: PathBuf,
    pub torrserver_dir: PathBuf,
    pub torrserver_exe: PathBuf,
    pub ts_cache_dir: PathBuf,
    pub ts_playlists_dir: PathBuf,
    pub ts_history_path: PathBuf,
    pub ts_torrents_meta_path: PathBuf,
    pub ts_torrents_links_path: PathBuf,
    pub ts_torrents_saved_path: PathBuf,
    pub ts_torrents_dir: PathBuf,
    pub ts_posters_dir: PathBuf,
    pub ts_webview_dir: PathBuf,
}

impl AppPaths {
    pub fn init(cli_args: &[String]) -> CoreResult<Self> {
        let exe_path = std::env::current_exe()?;
        let exe_dir = exe_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        let override_dir = parse_data_dir_arg(cli_args);
        let user_data_dir = override_dir.unwrap_or_else(|| exe_dir.join("UserData"));
        let legacy_data_dir = exe_dir.join("data");

        if !user_data_dir.exists() && legacy_data_dir.exists() {
            let _ = migrate_legacy_data(&legacy_data_dir, &user_data_dir);
        }

        Self::from_root(exe_path, exe_dir, user_data_dir)
    }

    pub fn from_root(exe_path: PathBuf, exe_dir: PathBuf, user_data_dir: PathBuf) -> CoreResult<Self> {
        let _ = fs::create_dir_all(&user_data_dir);
        let read_only = !probe_writable(&user_data_dir);

        let torrserver_dir = user_data_dir.join("torrserver");
        let ts_cache_dir = torrserver_dir.join("ts_cache");
        let ts_playlists_dir = torrserver_dir.join("playlists");
        let ts_torrents_dir = torrserver_dir.join("torrents");
        let ts_posters_dir = torrserver_dir.join("posters");
        let modules_dir = user_data_dir.join("modules");
        let logs_dir = user_data_dir.join("Logs");
        let exports_dir = user_data_dir.join("Exports");
        let backups_dir = user_data_dir.join("Backups");

        if !read_only {
            for dir in [
                &torrserver_dir,
                &ts_cache_dir,
                &ts_playlists_dir,
                &ts_torrents_dir,
                &ts_posters_dir,
                &modules_dir,
                &logs_dir,
                &exports_dir,
                &backups_dir,
            ] {
                let _ = fs::create_dir_all(dir);
            }
        }

        let settings_path = {
            let primary = user_data_dir.join("settings.json");
            let legacy = user_data_dir.join("config.json");
            if !primary.exists() && legacy.exists() && !read_only {
                let _ = fs::copy(&legacy, &primary);
            }
            primary
        };

        Ok(Self {
            exe_path,
            exe_dir,
            settings_path,
            db_path: user_data_dir.join("app_data.db"),
            secrets_path: user_data_dir.join("secrets.enc"),
            crash_log_path: logs_dir.join("crash.log"),
            logs_dir,
            exports_dir,
            backups_dir,
            modules_dir,
            torrserver_exe: torrserver_dir.join("TorrServer-windows-amd64.exe"),
            ts_history_path: torrserver_dir.join("history.json"),
            ts_torrents_meta_path: torrserver_dir.join("torrents_meta.json"),
            ts_torrents_links_path: torrserver_dir.join("torrents_links.json"),
            ts_torrents_saved_path: torrserver_dir.join("torrents_saved.json"),
            ts_torrents_dir,
            ts_posters_dir,
            ts_webview_dir: torrserver_dir.join("webview2"),
            user_data_dir,
            read_only,
            torrserver_dir,
            ts_cache_dir,
            ts_playlists_dir,
        })
    }
}

pub fn parse_data_dir_arg(args: &[String]) -> Option<PathBuf> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--data-dir" {
            if let Some(val) = iter.next() {
                if !val.trim().is_empty() {
                    return Some(PathBuf::from(val.trim()));
                }
            }
        } else if let Some(val) = arg.strip_prefix("--data-dir=") {
            if !val.trim().is_empty() {
                return Some(PathBuf::from(val.trim()));
            }
        }
    }
    None
}

fn probe_writable(dir: &Path) -> bool {
    let probe = dir.join(".write_probe.tmp");
    match fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn migrate_legacy_data(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    copy_dir_recursive(src, dst)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&target)?;
            copy_dir_recursive(&entry.path(), &target)?;
        } else if !target.exists() {
            let _ = fs::copy(entry.path(), target);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_data_dir_cli_variants() {
        let a1 = vec!["app.exe".into(), "--data-dir".into(), "D:\\Portable".into()];
        assert_eq!(parse_data_dir_arg(&a1), Some(PathBuf::from("D:\\Portable")));

        let a2 = vec!["app.exe".into(), "--data-dir=/tmp/ud".into()];
        assert_eq!(parse_data_dir_arg(&a2), Some(PathBuf::from("/tmp/ud")));
    }

    #[test]
    fn initializes_user_data_tree() {
        let base = std::env::temp_dir().join(format!("ts_paths_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let p = AppPaths::from_root(base.join("app.exe"), base.clone(), base.join("UserData"))
            .expect("paths init");
        assert!(!p.read_only);
        assert!(p.logs_dir.exists());
        assert!(p.backups_dir.exists());
        assert!(p.exports_dir.exists());
        let _ = fs::remove_dir_all(&base);
    }
}
