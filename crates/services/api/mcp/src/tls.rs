//! MRFC-0080 R3c — rustls configurations from PEM, shared by the
//! replication stream and the Kubernetes lease. The ring provider is
//! named explicitly (it is the one compiled in; no process default).

use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, ServerName};
use std::path::Path;
use std::sync::Arc;

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A client that trusts exactly the CA certificates in `ca_pem`.
pub(crate) fn client_config(ca_pem: &[u8]) -> Result<Arc<rustls::ClientConfig>, String> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(ca_pem) {
        let cert = cert.map_err(|e| format!("CA certificate: {e}"))?;
        roots
            .add(cert)
            .map_err(|e| format!("CA certificate: {e}"))?;
    }
    if roots.is_empty() {
        return Err("no CA certificate in the PEM".into());
    }
    let cfg = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

/// A server presenting the chain in `cert_pem` with the key in `key_pem`.
pub(crate) fn server_config(
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<Arc<rustls::ServerConfig>, String> {
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<_, _>>()
        .map_err(|e| format!("certificate: {e}"))?;
    if chain.is_empty() {
        return Err("no certificate in the PEM".into());
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| format!("private key: {e}"))?;
    let cfg = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|e| format!("certificate/key: {e}"))?;
    Ok(Arc::new(cfg))
}

pub(crate) fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(Path::new(path)).map_err(|e| format!("{path}: {e}"))
}

/// The name a "host:port" address is verified against (an IP or a DNS
/// name — the leader's certificate must carry it as a SAN).
pub(crate) fn server_name(addr: &str) -> Result<ServerName<'static>, String> {
    let host = addr.rsplit_once(':').map_or(addr, |(h, _)| h);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    ServerName::try_from(host.to_string())
        .map_err(|e| format!("{host:?} is not a TLS server name: {e}"))
}
