//! Persistent Ed25519 identity for a BroLink node.

use anyhow::{anyhow, Context, Result};
use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Identity {
    pub signing: SigningKey,
    pub public: [u8; 32],
}

impl Identity {
    pub fn generate() -> Self {
        let signing = SigningKey::generate(&mut rand::rngs::OsRng);
        let public = signing.verifying_key().to_bytes();
        Self { signing, public }
    }

    pub fn from_bytes(sk: [u8; 32]) -> Self {
        let signing = SigningKey::from_bytes(&sk);
        let public = signing.verifying_key().to_bytes();
        Self { signing, public }
    }

    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            let raw = fs::read(path).with_context(|| format!("read {}", path.display()))?;
            if raw.len() != 32 {
                anyhow::bail!("identity file {} is not 32 bytes", path.display());
            }
            let mut sk = [0u8; 32];
            sk.copy_from_slice(&raw);
            return Ok(Self::from_bytes(sk));
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let id = Self::generate();
        fs::write(path, id.signing.to_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }
        Ok(id)
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    pub fn short_id(&self) -> String {
        data_encoding::BASE32_NOPAD.encode(&self.public[..5])
    }

    pub fn public_hex(&self) -> String {
        data_encoding::HEXLOWER.encode(&self.public)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AllowList {
    pub clients: Vec<AllowedClient>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowedClient {
    pub public: String,
    pub name: String,
    pub added_unix: u64,
}

impl AllowList {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let s = fs::read_to_string(path)?;
        Ok(toml::from_str(&s)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn contains(&self, public: &[u8; 32]) -> bool {
        let hex = data_encoding::HEXLOWER.encode(public);
        self.clients.iter().any(|c| c.public == hex)
    }

    pub fn add(&mut self, public: &[u8; 32], name: &str) {
        if self.contains(public) {
            return;
        }
        self.clients.push(AllowedClient {
            public: data_encoding::HEXLOWER.encode(public),
            name: name.to_string(),
            added_unix: crate::proto::now_us() / 1_000_000,
        });
    }

    /// Forget a previously paired client. Returns true if it was present.
    pub fn remove(&mut self, public: &[u8; 32]) -> bool {
        let hex = data_encoding::HEXLOWER.encode(public);
        let before = self.clients.len();
        self.clients.retain(|c| c.public != hex);
        self.clients.len() != before
    }

    pub fn remove_hex(&mut self, hex: &str) -> bool {
        let hex = hex.to_ascii_lowercase();
        let before = self.clients.len();
        self.clients.retain(|c| c.public != hex);
        self.clients.len() != before
    }

    pub fn parse_public(hex: &str) -> Result<[u8; 32]> {
        let v = data_encoding::HEXLOWER
            .decode(hex.as_bytes())
            .or_else(|_| data_encoding::HEXLOWER_PERMISSIVE.decode(hex.as_bytes()))
            .map_err(|e| anyhow!(e))?;
        v.try_into()
            .map_err(|_| anyhow!("identity is not 32 bytes"))
    }
}

pub fn data_dir() -> Result<PathBuf> {
    let base = directories::ProjectDirs::from("dev", "BroLink", "BroLink")
        .ok_or_else(|| anyhow!("cannot resolve data dir"))?;
    Ok(base.config_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "brolink-test-{tag}-{}-{}",
            std::process::id(),
            crate::proto::now_us()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn identity_is_stable_across_loads() {
        let dir = temp_dir("id");
        let path = dir.join("nested").join("host.key");
        let a = Identity::load_or_create(&path).unwrap();
        assert!(path.exists(), "load_or_create must create missing parents");
        let b = Identity::load_or_create(&path).unwrap();
        assert_eq!(a.public, b.public, "reloading must not rotate the identity");
        assert_eq!(a.public_hex(), b.public_hex());
        assert_eq!(a.short_id().len(), 8);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_identity_file_is_an_error_not_a_silent_new_key() {
        let dir = temp_dir("corrupt");
        let path = dir.join("host.key");
        fs::write(&path, b"too short").unwrap();
        assert!(
            Identity::load_or_create(&path).is_err(),
            "silently minting a new identity would break every existing pairing"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn generated_identities_differ() {
        let a = Identity::generate();
        let b = Identity::generate();
        assert_ne!(a.public, b.public);
    }

    #[test]
    fn from_bytes_matches_the_key_it_was_built_from() {
        let a = Identity::generate();
        let b = Identity::from_bytes(a.signing.to_bytes());
        assert_eq!(a.public, b.public);
    }

    #[test]
    fn allowlist_add_is_idempotent_and_roundtrips() {
        let dir = temp_dir("allow");
        let path = dir.join("allowlist.toml");
        let key = [7u8; 32];
        let other = [8u8; 32];

        let mut list = AllowList::load(&path).unwrap();
        assert!(list.clients.is_empty(), "a missing file is an empty list");
        assert!(!list.contains(&key));

        list.add(&key, "Mac");
        list.add(&key, "Mac again");
        assert_eq!(list.clients.len(), 1, "adding twice must not duplicate");
        assert!(list.contains(&key));
        assert!(!list.contains(&other));

        list.save(&path).unwrap();
        let reloaded = AllowList::load(&path).unwrap();
        assert!(reloaded.contains(&key));
        assert_eq!(reloaded.clients[0].name, "Mac");

        let mut reloaded = reloaded;
        assert!(reloaded.remove(&key));
        assert!(!reloaded.contains(&key));
        assert!(!reloaded.remove(&key), "second remove is a no-op");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_public_accepts_hex_and_rejects_junk() {
        let id = Identity::generate();
        let hex = id.public_hex();
        assert_eq!(AllowList::parse_public(&hex).unwrap(), id.public);
        assert_eq!(
            AllowList::parse_public(&hex.to_uppercase()).unwrap(),
            id.public
        );
        assert!(AllowList::parse_public("nothex").is_err());
        assert!(AllowList::parse_public("aabb").is_err(), "wrong length");
    }
}
