//! Who this client is to a Sunshine host: an RSA key pair, a self-signed
//! certificate, and a 16-hex-digit id. Created once and kept in the data
//! directory; Sunshine remembers the certificate when pairing succeeds.

use anyhow::{Context, Result};
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use rsa::{Pkcs1v15Sign, RsaPrivateKey};
use sha2::{Digest, Sha256};
use std::path::Path;

pub struct Identity {
    pub unique_id: String,
    pub cert_pem: String,
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    key: RsaPrivateKey,
}

impl Identity {
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        // Existing client.key / client.crt / client.id are never rotated.
        // Regenerating them unpairs every PC; engine migration and debrand
        // must not touch these files.
        let key_path = dir.join("client.key");
        let cert_path = dir.join("client.crt");
        let id_path = dir.join("client.id");
        let (key_pem, cert_pem) = match (
            std::fs::read_to_string(&key_path),
            std::fs::read_to_string(&cert_path),
        ) {
            (Ok(k), Ok(c)) if !k.is_empty() && !c.is_empty() => (k, c),
            _ => {
                let (k, c) = generate()?;
                write_secret(&key_path, k.as_bytes())?;
                std::fs::write(&cert_path, &c)?;
                (k, c)
            }
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
        }
        let unique_id = match std::fs::read_to_string(&id_path) {
            Ok(s) if s.trim().len() == 16 => s.trim().to_string(),
            _ => {
                let id: String = (0..16)
                    .map(|_| {
                        let d = rand::random::<u8>() % 16;
                        char::from_digit(d as u32, 16).unwrap().to_ascii_uppercase()
                    })
                    .collect();
                std::fs::write(&id_path, &id)?;
                id
            }
        };
        let key = RsaPrivateKey::from_pkcs8_pem(&key_pem).context("client.key")?;
        let key_der = key.to_pkcs8_der()?.as_bytes().to_vec();
        let cert_der = pem_to_der(&cert_pem).context("client.crt")?;
        Ok(Self {
            unique_id,
            cert_pem,
            cert_der,
            key_der,
            key,
        })
    }

    /// The signature bytes of our certificate, an input to the pairing hash.
    pub fn cert_signature(&self) -> Result<Vec<u8>> {
        cert_signature(&self.cert_der)
    }

    /// RSA PKCS#1 v1.5 signature over SHA-256 of `data`.
    pub fn sign(&self, data: &[u8]) -> Result<Vec<u8>> {
        let digest = Sha256::digest(data);
        Ok(self.key.sign(Pkcs1v15Sign::new::<Sha256>(), &digest)?)
    }
}

/// The pairing private key is not world-readable.
fn write_secret(path: &Path, bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut f| f.write_all(bytes))
            .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}

/// A fresh 2048-bit RSA key and a certificate good for twenty years, both as
/// PEM. Sunshine only ever checks that the certificate presented on the
/// paired HTTPS channel is the one it saw during pairing.
fn generate() -> Result<(String, String)> {
    let key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048)?;
    let key_pem = key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)?.to_string();
    let pair = rcgen::KeyPair::from_pkcs8_pem_and_sign_algo(&key_pem, &rcgen::PKCS_RSA_SHA256)?;
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new())?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "BroLink");
    params.not_before = rcgen::date_time_ymd(2026, 1, 1);
    params.not_after = rcgen::date_time_ymd(2046, 1, 1);
    let cert = params.self_signed(&pair)?;
    Ok((key_pem, cert.pem()))
}

pub fn pem_to_der(pem: &str) -> Result<Vec<u8>> {
    let (_, p) = x509_parser::pem::parse_x509_pem(pem.as_bytes())
        .map_err(|e| anyhow::anyhow!("not PEM: {e}"))?;
    Ok(p.contents)
}

pub fn cert_signature(der: &[u8]) -> Result<Vec<u8>> {
    let (_, cert) =
        x509_parser::parse_x509_certificate(der).map_err(|e| anyhow::anyhow!("x509: {e}"))?;
    Ok(cert.signature_value.data.as_ref().to_vec())
}

/// Verify an RSA PKCS#1 v1.5 SHA-256 signature made by the holder of `der`.
pub fn verify(der: &[u8], data: &[u8], signature: &[u8]) -> Result<()> {
    use rsa::pkcs8::DecodePublicKey;
    let (_, cert) =
        x509_parser::parse_x509_certificate(der).map_err(|e| anyhow::anyhow!("x509: {e}"))?;
    let key = rsa::RsaPublicKey::from_public_key_der(cert.public_key().raw)?;
    let digest = Sha256::digest(data);
    key.verify(Pkcs1v15Sign::new::<Sha256>(), &digest, signature)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_created_once_and_signs_verifiably() {
        let dir = std::env::temp_dir().join(format!("brolink-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = Identity::load_or_create(&dir).unwrap();
        let key_bytes = std::fs::read(dir.join("client.key")).unwrap();
        let crt_bytes = std::fs::read(dir.join("client.crt")).unwrap();
        let id_bytes = std::fs::read(dir.join("client.id")).unwrap();
        let b = Identity::load_or_create(&dir).unwrap();
        assert_eq!(std::fs::read(dir.join("client.key")).unwrap(), key_bytes);
        assert_eq!(std::fs::read(dir.join("client.crt")).unwrap(), crt_bytes);
        assert_eq!(std::fs::read(dir.join("client.id")).unwrap(), id_bytes);
        assert_eq!(a.unique_id, b.unique_id);
        assert_eq!(a.cert_der, b.cert_der);
        assert_eq!(a.unique_id.len(), 16);
        let sig = a.sign(b"secret").unwrap();
        assert_eq!(sig.len(), 256);
        verify(&a.cert_der, b"secret", &sig).unwrap();
        assert!(verify(&a.cert_der, b"other", &sig).is_err());
        assert!(!a.cert_signature().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
