//! Where each app keeps its few settings: one TOML file in the per-user data
//! directory (`%LOCALAPPDATA%\Latch` on Windows, `~/Library/Application
//! Support/com.bardbro.Latch` on macOS, `~/.local/share/latch` on Linux).

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex, Once,
};

static CONFIG_LOCK: Mutex<()> = Mutex::new(());
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// The per-user data directory, created private to this user.
///
/// `LATCH_DATA_DIR` replaces it when set. The workspace's
/// `.cargo/config.toml` sets it for `cargo test` and `cargo run`, so a test
/// run on a machine with Latch installed leaves that install's settings,
/// pairing identity and logs alone.
///
/// The first call by a copy that finds the folder 4.0 and older used, and
/// none of its own, takes that folder over: see [`adopt`].
pub fn data_dir() -> Result<PathBuf> {
    let explicit = crate::legacy::env("LATCH_DATA_DIR").map(PathBuf::from);
    let adopting = explicit.is_none();
    let dir = if let Some(dir) = explicit {
        Some(dir)
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|p| p.join(crate::APP_NAME))
    } else {
        directories::ProjectDirs::from("com", "bardbro", crate::APP_NAME)
            .map(|d| d.data_dir().to_path_buf())
    }
    .context("no per-user data directory")?;
    if adopting {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            if let Some(old) = crate::legacy::data_dir() {
                match adopt(&old, &dir) {
                    Ok(true) => tracing::info!("took over {} as {}", old.display(), dir.display()),
                    Ok(false) => {}
                    Err(e) => tracing::warn!("could not take over {}: {e:#}", old.display()),
                }
            }
        });
    }
    std::fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

/// Make the folder an older install left at `old` the one at `new`, when
/// `old` exists and `new` does not. Settings, the pairing identity, the
/// paired devices and the streaming engine all come along. Returns whether
/// it did.
///
/// A folder is moved. On Windows the old one holds the running
/// `latch-host.exe` of an install that has not been set up again, and
/// Windows will not move a folder with a running program in it, so it is
/// copied without its executables and the old one is left for setup to
/// remove.
pub fn adopt(old: &Path, new: &Path) -> Result<bool> {
    if new.exists() || !old.is_dir() {
        return Ok(false);
    }
    if let Some(parent) = new.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !cfg!(windows) && std::fs::rename(old, new).is_ok() {
        return Ok(true);
    }
    // Staged beside the destination, so that a copy cut short never leaves a
    // half-filled folder that the next start mistakes for the real one.
    let stage = new.with_extension(format!("adopting-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    let copied = copy_tree(old, &stage)
        .and_then(|()| std::fs::rename(&stage, new).with_context(|| new.display().to_string()));
    if copied.is_err() {
        let _ = std::fs::remove_dir_all(&stage);
    }
    copied.map(|()| true)
}

/// Copy `from` to `to`, leaving out programs and the swap files an update
/// makes of them. A file that cannot be read (another process holds it
/// open) is skipped, not fatal: it is a log, not a setting.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let lower = name.to_string_lossy().to_ascii_lowercase();
        let src = entry.path();
        let dst = to.join(&name);
        if entry.file_type()?.is_dir() {
            copy_tree(&src, &dst)?;
        } else if lower.ends_with(".exe")
            || lower.ends_with(".exe.old")
            || lower.ends_with(".exe.new")
        {
            continue;
        } else if let Err(e) = std::fs::copy(&src, &dst) {
            tracing::warn!("skipped {}: {e}", src.display());
        }
    }
    Ok(())
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
        // Loading junk moves it aside as `<stem>.bad-*`, exactly once.
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

    /// `.cargo/config.toml` points `cargo test` at `target/latch-data`.
    /// Without it every test that loads or saves settings would read and
    /// rewrite the real install's files on the machine running the tests.
    #[test]
    fn tests_run_against_a_data_directory_under_target() {
        let want = std::env::var_os("LATCH_DATA_DIR").expect("cargo test sets LATCH_DATA_DIR");
        let dir = data_dir().unwrap();
        assert_eq!(dir, PathBuf::from(want));
        assert!(
            dir.components().any(|c| c.as_os_str() == "target"),
            "{}",
            dir.display()
        );
    }

    #[test]
    fn a_new_install_takes_over_the_folder_an_older_one_left() {
        let root = std::env::temp_dir().join(format!("latch-adopt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let old = root.join("old");
        let new = root.join("nested/new");
        std::fs::create_dir_all(old.join("engine/config")).unwrap();
        std::fs::write(old.join("host.toml"), "sunshine_user = \"brolink\"\n").unwrap();
        std::fs::write(old.join("engine/config/state.json"), "{}").unwrap();
        std::fs::write(old.join("brolink-host.exe"), "program").unwrap();

        assert!(adopt(&old, &new).unwrap());
        assert_eq!(
            std::fs::read_to_string(new.join("host.toml")).unwrap(),
            "sunshine_user = \"brolink\"\n"
        );
        assert!(new.join("engine/config/state.json").exists());
        if cfg!(windows) {
            // Copied: programs stay behind in the old folder.
            assert!(!new.join("brolink-host.exe").exists());
            assert!(old.join("host.toml").exists());
        } else {
            assert!(new.join("brolink-host.exe").exists());
            assert!(!old.exists());
        }

        // Once there is a folder of its own, nothing is taken again.
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("host.toml"), "other").unwrap();
        assert!(!adopt(&old, &new).unwrap());
        assert_ne!(
            std::fs::read_to_string(new.join("host.toml")).unwrap(),
            "other"
        );
        // No old folder, nothing to take.
        assert!(!adopt(&root.join("missing"), &root.join("elsewhere")).unwrap());
        assert!(!root.join("elsewhere").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn copying_a_folder_leaves_programs_out_and_is_staged() {
        let root = std::env::temp_dir().join(format!("latch-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let old = root.join("old");
        std::fs::create_dir_all(&old).unwrap();
        for f in ["a.toml", "x.exe", "x.exe.old", "x.exe.new", "X.EXE"] {
            std::fs::write(old.join(f), f).unwrap();
        }
        copy_tree(&old, &root.join("new")).unwrap();
        let mut got: Vec<String> = std::fs::read_dir(root.join("new"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        got.sort();
        assert_eq!(got, ["a.toml"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn machine_name_is_never_empty() {
        assert!(!machine_name().is_empty());
    }
}
