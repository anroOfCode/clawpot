use anyhow::{Context, Result};
use rustls::ServerConfig;
use std::io::Cursor;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tracing::{error, info, warn};

use super::ca::CertificateAuthority;

const MITM_LISTEN_ADDR: &str = "0.0.0.0:10443";
const HTTP_PROXY_TLS_ADDR: &str = "127.0.0.1:10081";

/// Start the TLS MITM proxy. Runs until the cancellation token is triggered.
pub async fn run(
    ca: Arc<CertificateAuthority>,
    cancel: tokio::sync::watch::Receiver<bool>,
    ready: tokio::sync::oneshot::Sender<()>,
) {
    match run_inner(ca, cancel, ready).await {
        Ok(()) => info!("TLS MITM proxy shut down"),
        Err(e) => error!("TLS MITM proxy failed: {:#}", e),
    }
}

async fn run_inner(
    ca: Arc<CertificateAuthority>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    ready: tokio::sync::oneshot::Sender<()>,
) -> Result<()> {
    let listener = TcpListener::bind(MITM_LISTEN_ADDR)
        .await
        .with_context(|| format!("Failed to bind TLS MITM proxy on {MITM_LISTEN_ADDR}"))?;

    info!("TLS MITM proxy listening on {}", MITM_LISTEN_ADDR);

    // Signal readiness now that the socket is bound
    let _ = ready.send(());

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, addr) = result.context("Failed to accept connection")?;
                let ca = ca.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, ca).await {
                        warn!("MITM connection from {} failed: {:#}", addr, e);
                    }
                });
            }
            _ = cancel.changed() => {
                info!("TLS MITM proxy received shutdown signal");
                break;
            }
        }
    }

    Ok(())
}

async fn handle_connection(stream: TcpStream, ca: Arc<CertificateAuthority>) -> Result<()> {
    // Peek at the TLS ClientHello to extract SNI
    let mut buf = vec![0u8; 4096];
    let n = stream
        .peek(&mut buf)
        .await
        .context("Failed to peek at TLS ClientHello")?;
    let sni = extract_sni(&buf[..n]).unwrap_or_default();

    if sni.is_empty() {
        anyhow::bail!("No SNI found in ClientHello");
    }

    // Generate a leaf cert for this domain
    let leaf = ca
        .get_or_create_cert(&sni)
        .await
        .with_context(|| format!("Failed to generate cert for {sni}"))?;

    // Build rustls server config with the leaf cert
    let tls_config = build_server_config(&leaf.cert_pem, &leaf.key_pem, ca.ca_cert_pem())
        .with_context(|| format!("Failed to build TLS config for {sni}"))?;

    let acceptor = TlsAcceptor::from(Arc::new(tls_config));
    let tls_stream = acceptor
        .accept(stream)
        .await
        .context("TLS handshake failed")?;

    // Connect to HTTP proxy's TLS-upstream listener
    let proxy_stream = TcpStream::connect(HTTP_PROXY_TLS_ADDR)
        .await
        .with_context(|| format!("Failed to connect to HTTP proxy at {HTTP_PROXY_TLS_ADDR}"))?;

    // Bidirectional copy between TLS stream and HTTP proxy
    let (mut tls_read, mut tls_write) = tokio::io::split(tls_stream);
    let (mut proxy_read, mut proxy_write) = tokio::io::split(proxy_stream);

    let c2e = tokio::spawn(async move { copy(&mut tls_read, &mut proxy_write).await });
    let e2c = tokio::spawn(async move { copy(&mut proxy_read, &mut tls_write).await });

    let _ = tokio::try_join!(c2e, e2c);
    Ok(())
}

async fn copy<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
) -> Result<()> {
    tokio::io::copy(reader, writer).await?;
    writer.shutdown().await?;
    Ok(())
}

/// Build a rustls ServerConfig from PEM cert chain and private key.
fn build_server_config(cert_pem: &str, key_pem: &str, ca_pem: &str) -> Result<ServerConfig> {
    // Parse leaf cert
    let mut cert_reader = Cursor::new(cert_pem);
    let mut certs: Vec<_> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Failed to parse leaf cert PEM")?;

    // Append CA cert so the chain is complete
    let mut ca_reader = Cursor::new(ca_pem);
    let ca_certs: Vec<_> = rustls_pemfile::certs(&mut ca_reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Failed to parse CA cert PEM")?;
    certs.extend(ca_certs);

    // Parse private key
    let mut key_reader = Cursor::new(key_pem);
    let key = rustls_pemfile::private_key(&mut key_reader)
        .context("Failed to read private key PEM")?
        .context("No private key found in PEM")?;

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("Failed to build ServerConfig")?;

    Ok(config)
}

/// Extract SNI hostname from a TLS ClientHello message.
/// Returns None if parsing fails or no SNI extension is present.
fn extract_sni(buf: &[u8]) -> Option<String> {
    // TLS record: type(1) + version(2) + length(2) + handshake
    if buf.len() < 5 || buf[0] != 0x16 {
        return None; // Not a TLS handshake
    }

    let record_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    let handshake = &buf[5..buf.len().min(5 + record_len)];

    // Handshake: type(1) + length(3) + ClientHello
    if handshake.is_empty() || handshake[0] != 0x01 {
        return None; // Not ClientHello
    }

    let hs_len = u32::from_be_bytes([0, handshake[1], handshake[2], handshake[3]]) as usize;
    let client_hello = &handshake[4..handshake.len().min(4 + hs_len)];

    // ClientHello: version(2) + random(32) + session_id(1+var) + cipher_suites(2+var) + compression(1+var) + extensions
    if client_hello.len() < 34 {
        return None;
    }

    let mut pos = 34; // skip version + random

    // Session ID
    if pos >= client_hello.len() {
        return None;
    }
    let session_id_len = client_hello[pos] as usize;
    pos += 1 + session_id_len;

    // Cipher suites
    if pos + 2 > client_hello.len() {
        return None;
    }
    let cipher_suites_len = u16::from_be_bytes([client_hello[pos], client_hello[pos + 1]]) as usize;
    pos += 2 + cipher_suites_len;

    // Compression methods
    if pos >= client_hello.len() {
        return None;
    }
    let compression_len = client_hello[pos] as usize;
    pos += 1 + compression_len;

    // Extensions length
    if pos + 2 > client_hello.len() {
        return None;
    }
    let extensions_len = u16::from_be_bytes([client_hello[pos], client_hello[pos + 1]]) as usize;
    pos += 2;

    let extensions_end = pos + extensions_len.min(client_hello.len() - pos);

    // Walk extensions looking for SNI (type 0x0000)
    while pos + 4 <= extensions_end {
        let ext_type = u16::from_be_bytes([client_hello[pos], client_hello[pos + 1]]);
        let ext_len = u16::from_be_bytes([client_hello[pos + 2], client_hello[pos + 3]]) as usize;
        pos += 4;

        if ext_type == 0x0000 {
            // SNI extension
            // server_name_list_length(2) + server_name_type(1) + host_name_length(2) + host_name
            if ext_len >= 5 && pos + ext_len <= extensions_end {
                let name_type = client_hello[pos + 2];
                if name_type == 0x00 {
                    // host_name
                    let name_len =
                        u16::from_be_bytes([client_hello[pos + 3], client_hello[pos + 4]]) as usize;
                    if pos + 5 + name_len <= extensions_end {
                        let name = &client_hello[pos + 5..pos + 5 + name_len];
                        return String::from_utf8(name.to_vec()).ok();
                    }
                }
            }
            return None;
        }

        pos += ext_len;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_sni_none_for_non_tls() {
        assert_eq!(extract_sni(b"GET / HTTP/1.1\r\n"), None);
        assert_eq!(extract_sni(&[]), None);
    }
}
