//! The host's few settings, in `%LOCALAPPDATA%\BroLink\host.toml`.

use serde::{Deserialize, Serialize};

pub const FILE: &str = "host.toml";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HostConfig {
    /// Let a Mac on the tailnet sleep, restart, or shut this PC down.
    pub power_allowed: bool,
    /// Sunshine web-UI login the host uses to accept pairing PINs. Written
    /// by the setup step, which sets the same values on Sunshine.
    pub sunshine_user: String,
    pub sunshine_pass: String,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            power_allowed: true,
            sunshine_user: String::new(),
            sunshine_pass: String::new(),
        }
    }
}

impl HostConfig {
    pub fn load() -> Self {
        brolink_core::config::load(FILE)
    }
    pub fn save(&self) -> anyhow::Result<()> {
        brolink_core::config::save(FILE, self)
    }
    pub fn has_creds(&self) -> bool {
        !self.sunshine_user.is_empty() && !self.sunshine_pass.is_empty()
    }
}

/// A fresh login for Sunshine's web UI: letters and digits only, so it
/// survives every quoting layer between here and `sunshine --creds`.
pub fn random_password() -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"abcdefghjkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    (0..20)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_is_long_and_quote_safe() {
        let p = random_password();
        assert_eq!(p.len(), 20);
        assert!(p.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(p, random_password());
    }

    #[test]
    fn defaults_allow_power_but_have_no_creds() {
        let c = HostConfig::default();
        assert!(c.power_allowed);
        assert!(!c.has_creds());
    }
}
