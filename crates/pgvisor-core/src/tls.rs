use std::fs::{self, File, Permissions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
pub enum TlsCertError {
    #[error("I/O error during TLS cert handling: {0}")]
    Io(#[from] std::io::Error),

    #[error("Certificate generation error: {0}")]
    Generation(String),
}

/// A PEM-encoded certificate and private key pair.
#[derive(Debug, Clone)]
pub struct TlsCertPair {
    pub cert_pem: String,
    pub key_pem: String,
}

impl TlsCertPair {
    /// Generates a self-signed certificate for the given common name and optional subject alt names.
    pub fn generate_self_signed(
        common_name: &str,
        subject_alt_names: &[String],
    ) -> Result<Self, TlsCertError> {
        let mut san_list = vec![common_name.to_string()];
        for san in subject_alt_names {
            if !san_list.contains(san) {
                san_list.push(san.clone());
            }
        }

        let mut params = rcgen::CertificateParams::new(san_list)
            .map_err(|e| TlsCertError::Generation(e.to_string()))?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, common_name);

        let key_pair =
            rcgen::KeyPair::generate().map_err(|e| TlsCertError::Generation(e.to_string()))?;
        let cert = params
            .self_signed(&key_pair)
            .map_err(|e| TlsCertError::Generation(e.to_string()))?;

        let cert_pem = cert.pem();
        let key_pem = key_pair.serialize_pem();

        Ok(Self { cert_pem, key_pem })
    }

    /// Loads an existing cert and key from paths if both exist,
    /// or generates a new self-signed cert, writes them with secure permissions (0600 on key), and returns it.
    pub fn load_or_generate(
        cert_path: impl AsRef<Path>,
        key_path: impl AsRef<Path>,
        common_name: &str,
        subject_alt_names: &[String],
    ) -> Result<Self, TlsCertError> {
        let cert_p = cert_path.as_ref();
        let key_p = key_path.as_ref();

        if cert_p.exists() && key_p.exists() {
            let cert_pem = fs::read_to_string(cert_p)?;
            let key_pem = fs::read_to_string(key_p)?;
            info!(cert = ?cert_p, "Loaded existing TLS certificate and private key");
            return Ok(Self { cert_pem, key_pem });
        }

        info!(
            cert = ?cert_p,
            key = ?key_p,
            common_name,
            "Generating new self-signed TLS certificate"
        );

        let pair = Self::generate_self_signed(common_name, subject_alt_names)?;

        if let Some(parent) = cert_p.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Some(parent) = key_p.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut c_file = File::create(cert_p)?;
        c_file.write_all(pair.cert_pem.as_bytes())?;
        c_file.sync_all()?;
        fs::set_permissions(cert_p, Permissions::from_mode(0o644))?;

        let mut k_file = File::create(key_p)?;
        k_file.write_all(pair.key_pem.as_bytes())?;
        k_file.sync_all()?;
        // PostgreSQL strictly requires private key file to have 0600 permissions
        fs::set_permissions(key_p, Permissions::from_mode(0o600))?;

        info!(
            cert = ?cert_p,
            key = ?key_p,
            "Saved self-signed TLS certificate and private key"
        );

        Ok(pair)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_generate_self_signed() {
        let pair = TlsCertPair::generate_self_signed("localhost", &["127.0.0.1".into()]).unwrap();
        assert!(pair.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(pair.key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn test_load_or_generate() {
        let dir = tempdir().unwrap();
        let cert_path = dir.path().join("server.crt");
        let key_path = dir.path().join("server.key");

        // 1. First call generates
        let pair1 = TlsCertPair::load_or_generate(
            &cert_path,
            &key_path,
            "pgvisor-node1",
            &["localhost".into()],
        )
        .unwrap();

        assert!(cert_path.exists());
        assert!(key_path.exists());

        // Check key permissions
        let meta = fs::metadata(&key_path).unwrap();
        let perm = meta.permissions().mode() & 0o777;
        assert_eq!(perm, 0o600);

        // 2. Second call loads existing
        let pair2 = TlsCertPair::load_or_generate(
            &cert_path,
            &key_path,
            "pgvisor-node1",
            &["localhost".into()],
        )
        .unwrap();

        assert_eq!(pair1.cert_pem, pair2.cert_pem);
        assert_eq!(pair1.key_pem, pair2.key_pem);
    }
}
