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

/// One served TLS connection split for the client listeners' model — a
/// reader thread blocked on the socket while responses are written from
/// others. rustls state lives behind one lock; the reader waits for
/// ciphertext with `peek` outside it, so a write never queues behind an
/// idle read, and the shutdown drain's socket close still wakes it.
pub(crate) struct TlsReader {
    conn: Arc<std::sync::Mutex<rustls::ServerConnection>>,
    sock: std::net::TcpStream,
}

pub(crate) struct TlsWriter {
    conn: Arc<std::sync::Mutex<rustls::ServerConnection>>,
    sock: std::net::TcpStream,
}

/// The handshake, then the reader/writer pair. The handshake is bounded
/// (a silent peer cannot hold the handler thread); the socket's previous
/// read timeout is restored after it.
pub(crate) fn accept(
    sock: std::net::TcpStream,
    cfg: Arc<rustls::ServerConfig>,
) -> std::io::Result<(TlsReader, TlsWriter)> {
    let mut conn = rustls::ServerConnection::new(cfg).map_err(std::io::Error::other)?;
    let before = sock.read_timeout()?;
    sock.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    while conn.is_handshaking() {
        conn.complete_io(&mut &sock)?;
    }
    sock.set_read_timeout(before)?;
    let conn = Arc::new(std::sync::Mutex::new(conn));
    let reader = TlsReader {
        conn: conn.clone(),
        sock: sock.try_clone()?,
    };
    Ok((reader, TlsWriter { conn, sock }))
}

/// Everything rustls has queued (handshake replies, alerts, records) out.
fn flush_tls(
    conn: &mut rustls::ServerConnection,
    sock: &std::net::TcpStream,
) -> std::io::Result<()> {
    while conn.wants_write() {
        conn.write_tls(&mut &*sock)?;
    }
    Ok(())
}

impl std::io::Read for TlsReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::ErrorKind;
        loop {
            {
                let mut conn = self.conn.lock().unwrap(); // justified: Mutex poison is unrecoverable
                match conn.reader().read(buf) {
                    Ok(n) => return Ok(n), // 0: the peer's close_notify
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                }
            }
            // no plaintext yet: wait for ciphertext without holding the lock
            if self.sock.peek(&mut [0u8; 1])? == 0 {
                return Ok(0);
            }
            let mut conn = self.conn.lock().unwrap(); // justified: Mutex poison is unrecoverable
            if conn.read_tls(&mut &self.sock)? == 0 {
                return Ok(0);
            }
            if let Err(e) = conn.process_new_packets() {
                let _ = flush_tls(&mut conn, &self.sock); // the alert, best effort
                return Err(std::io::Error::new(ErrorKind::InvalidData, e));
            }
            flush_tls(&mut conn, &self.sock)?;
        }
    }
}

impl std::io::Write for TlsWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut conn = self.conn.lock().unwrap(); // justified: Mutex poison is unrecoverable
        let n = conn.writer().write(buf)?;
        flush_tls(&mut conn, &self.sock)?;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut conn = self.conn.lock().unwrap(); // justified: Mutex poison is unrecoverable
        conn.writer().flush()?;
        flush_tls(&mut conn, &self.sock)
    }
}

/// A client connection's read and write halves, plaintext or TLS.
pub(crate) type Halves = (
    Box<dyn std::io::Read + Send>,
    Box<dyn std::io::Write + Send>,
);

/// Split an accepted client socket: a plain clone pair, or the TLS pair.
pub(crate) fn split(
    sock: std::net::TcpStream,
    cfg: Option<&Arc<rustls::ServerConfig>>,
) -> std::io::Result<Halves> {
    match cfg {
        None => Ok((Box::new(sock.try_clone()?), Box::new(sock))),
        Some(cfg) => {
            let (r, w) = accept(sock, cfg.clone())?;
            Ok((Box::new(r), Box::new(w)))
        }
    }
}
