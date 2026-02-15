use anyhow::{Context, Result};
use rcgen::{CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, BasicConstraints};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

/// Certificate authority that generates per-domain leaf certificates for TLS MITM.
pub struct CertificateAuthority {
    ca_cert: rcgen::Certificate,
    ca_key: KeyPair,
    ca_cert_pem: String,
    cache: Arc<Mutex<HashMap<String, CachedCert>>>,
    ca_dir: PathBuf,
}

/// A cached leaf certificate and its private key.
#[derive(Clone)]
pub struct CachedCert {
    pub cert_pem: String,
    pub key_pem: String,
}

impl CertificateAuthority {
    /// Create or load a CA from the given directory.
    /// If ca.crt and ca.key exist, loads them; otherwise generates new ones.
    pub fn new(ca_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(ca_dir)
            .with_context(|| format!("Failed to create CA directory: {}", ca_dir.display()))?;

        let cert_path = ca_dir.join("ca.crt");
        let key_path = ca_dir.join("ca.key");

        let (ca_cert, ca_key, ca_cert_pem) = if cert_path.exists() && key_path.exists() {
            info!("Loading existing CA from {}", ca_dir.display());
            let key_pem = std::fs::read_to_string(&key_path)
                .context("Failed to read CA key")?;
            let ca_cert_pem = std::fs::read_to_string(&cert_path)
                .context("Failed to read CA cert")?;

            let ca_key = KeyPair::from_pem(&key_pem)
                .context("Failed to parse CA key")?;

            // Re-generate the CA cert with the same key (rcgen 0.13 doesn't support
            // loading existing certs, but the key is what matters for signing).
            // We keep the original on-disk cert for the TLS chain so it matches
            // the cert injected into VM trust stores by setup-rootfs.sh.
            let mut params = CertificateParams::default();
            params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, "Clawpot MITM CA");
            dn.push(DnType::OrganizationName, "Clawpot");
            params.distinguished_name = dn;

            let ca_cert = params.self_signed(&ca_key)
                .context("Failed to self-sign CA cert")?;

            (ca_cert, ca_key, ca_cert_pem)
        } else {
            info!("Generating new CA certificate in {}", ca_dir.display());
            let ca_key = KeyPair::generate()
                .context("Failed to generate CA key pair")?;

            let mut params = CertificateParams::default();
            params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, "Clawpot MITM CA");
            dn.push(DnType::OrganizationName, "Clawpot");
            params.distinguished_name = dn;

            let ca_cert = params.self_signed(&ca_key)
                .context("Failed to self-sign CA cert")?;

            let ca_cert_pem = ca_cert.pem();
            let ca_key_pem = ca_key.serialize_pem();

            std::fs::write(&cert_path, &ca_cert_pem)
                .context("Failed to write CA cert")?;
            std::fs::write(&key_path, &ca_key_pem)
                .context("Failed to write CA key")?;

            info!("CA certificate written to {}", cert_path.display());
            (ca_cert, ca_key, ca_cert_pem)
        };

        Ok(Self {
            ca_cert,
            ca_key,
            ca_cert_pem,
            cache: Arc::new(Mutex::new(HashMap::new())),
            ca_dir: ca_dir.to_path_buf(),
        })
    }

    /// Get the CA certificate PEM string (for injecting into rootfs trust store).
    pub fn ca_cert_pem(&self) -> &str {
        &self.ca_cert_pem
    }

    /// Get the CA directory path.
    pub fn ca_dir(&self) -> &Path {
        &self.ca_dir
    }

    /// Generate (or retrieve cached) a leaf certificate for the given domain,
    /// signed by this CA.
    pub async fn get_or_create_cert(&self, domain: &str) -> Result<CachedCert> {
        let mut cache = self.cache.lock().await;
        if let Some(cached) = cache.get(domain) {
            return Ok(cached.clone());
        }

        let leaf_key = KeyPair::generate()
            .context("Failed to generate leaf key pair")?;

        let mut params = CertificateParams::new(vec![domain.to_string()])
            .context("Failed to create leaf cert params")?;
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, domain);
        params.distinguished_name = dn;

        let leaf_cert = params.signed_by(&leaf_key, &self.ca_cert, &self.ca_key)
            .context("Failed to sign leaf cert")?;

        let cached = CachedCert {
            cert_pem: leaf_cert.pem(),
            key_pem: leaf_key.serialize_pem(),
        };

        cache.insert(domain.to_string(), cached.clone());
        Ok(cached)
    }
}
