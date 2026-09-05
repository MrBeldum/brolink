//! Where each app keeps its few settings: one TOML file in the per-user data
//! directory (`%LOCALAPPDATA%\BroLink` on Windows, `~/Library/Application
//! Support/BroLink` on macOS).

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::path::PathBuf;

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
    Ok(dir)
}

/// Read `name` from the data directory; defaults when the file is missing
/// or damaged, because refusing to start over a setting is worse than
/// re-picking it.
pub fn load<T: DeserializeOwned + Default>(name: &str) -> T {
    let Ok(path) = data_dir().map(|d| d.join(name)) else {
        return T::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
            tracing::warn!("{} is unreadable ({e}); using defaults", path.display());
            T::default()
        }),
        Err(_) => T::default(),
    }
}

pub fn save<T: Serialize>(name: &str, value: &T) -> Result<()> {
    let path = data_dir()?.join(name);
    let text = toml::to_string_pretty(value)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path).with_context(|| path.display().to_string())
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
        let name = format!("test-{}.toml", std::process::id());
        save(&name, &Cfg { n: 7 }).unwrap();
        assert_eq!(load::<Cfg>(&name), Cfg { n: 7 });
        std::fs::write(data_dir().unwrap().join(&name), "not = [toml").unwrap();
        assert_eq!(load::<Cfg>(&name), Cfg::default());
        let _ = std::fs::remove_file(data_dir().unwrap().join(&name));
        assert_eq!(load::<Cfg>("does-not-exist.toml"), Cfg::default());
    }

    #[test]
    fn machine_name_is_never_empty() {
        assert!(!machine_name().is_empty());
    }
}
