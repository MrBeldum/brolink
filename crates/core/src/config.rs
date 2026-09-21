//! Where each app keeps its few settings: one TOML file in the per-user data
//! directory (`%LOCALAPPDATA%\BroLink` on Windows, `~/Library/Application
//! Support/BroLink` on macOS).

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

static CONFIG_LOCK: Mutex<()> = Mutex::new(());
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

pub fn data_dir() -> Result<PathBuf> {
    let dir = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|p| p.join(crate::APP_NAME))
    } else {
        directories::ProjectDirs::from("dev", "brolink", crate::APP_NAME)
            .map(|d| d.data_dir().to_path_buf())
    }
    .context("no per-user data directory")?;
    std::fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

/// Read `name` from the data directory; defaults when the file is missing
/// or damaged, because refusing to start over a setting is worse than
/// re-picking it.
pub fn load<T: DeserializeOwned + Default>(name: &str) -> T {
    let _guard = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load_unlocked(name)
}

fn load_unlocked<T: DeserializeOwned + Default>(name: &str) -> T {
    let Ok(path) = data_dir().map(|d| d.join(name)) else {
        return T::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
            tracing::warn!("{} is unreadable ({e}); using defaults", path.display());
            let backup = path.with_extension(format!(
                "bad-{}-{}",
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            ));
            if let Err(error) = std::fs::rename(&path, &backup) {
                tracing::warn!("could not preserve damaged config: {error}");
            } else {
                tracing::warn!("damaged config preserved at {}", backup.display());
            }
            T::default()
        }),
        Err(_) => T::default(),
    }
}

/// Apply a read-modify-write transaction without allowing another in-process
/// settings, discovery, or pairing writer to replace the snapshot underneath it.
pub fn update<T: Serialize + DeserializeOwned + Default + PartialEq>(
    name: &str,
    edit: impl FnOnce(&mut T),
) -> Result<()> {
    let _guard = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut value: T = load_unlocked(name);
    let before = toml::to_string(&value)?;
    edit(&mut value);
    if toml::to_string(&value)? != before {
        save_unlocked(name, &value)?;
    }
    Ok(())
}

pub fn save<T: Serialize>(name: &str, value: &T) -> Result<()> {
    let _guard = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    save_unlocked(name, value)
}

fn save_unlocked<T: Serialize>(name: &str, value: &T) -> Result<()> {
    let path = data_dir()?.join(name);
    let text = toml::to_string_pretty(value)?;
    let tmp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, &path).with_context(|| path.display().to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// This machine's name as the OS shows it.
pub fn machine_name() -> String {
    #[cfg(windows)]
    if let Some(n) = std::env::var_os("COMPUTERNAME") {
        let n = n.to_string_lossy().trim().to_string();
        if !n.is_empty() {
            return n;
        }
    }
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
    {
        let n = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !n.is_empty() {
            return n;
        }
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .trim_end_matches(".local")
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "This computer".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
    struct Cfg {
        n: u32,
    }

    #[test]
    fn save_then_load_round_trips_and_junk_falls_back() {
        let stem = format!("test-{}", std::process::id());
        let name = format!("{stem}.toml");
        let dir = data_dir().unwrap();
        save(&name, &Cfg { n: 7 }).unwrap();
        assert_eq!(load::<Cfg>(&name), Cfg { n: 7 });
        std::fs::write(dir.join(&name), "not = [toml").unwrap();
        assert_eq!(load::<Cfg>(&name), Cfg::default());
        // Loading junk moves it aside as `<stem>.bad-*`. This runs against the
        // user's real data directory, so remove what the test left behind.
        let prefix = format!("{stem}.bad-");
        let preserved: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix))
            })
            .collect();
        assert_eq!(
            preserved.len(),
            1,
            "damaged config is preserved exactly once"
        );
        for path in preserved {
            std::fs::remove_file(path).unwrap();
        }
        assert!(!dir.join(&name).exists());
        assert_eq!(load::<Cfg>("does-not-exist.toml"), Cfg::default());
    }

    #[test]
    fn concurrent_transactions_preserve_all_updates_and_private_permissions() {
        let name = format!("transaction-test-{}.toml", std::process::id());
        save(&name, &Cfg { n: 0 }).unwrap();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let name = &name;
                scope.spawn(move || {
                    for _ in 0..10 {
                        update::<Cfg>(name, |c| c.n += 1).unwrap();
                    }
                });
            }
        });
        assert_eq!(load::<Cfg>(&name).n, 80);
        let path = data_dir().unwrap().join(&name);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn machine_name_is_never_empty() {
        assert!(!machine_name().is_empty());
    }
}
