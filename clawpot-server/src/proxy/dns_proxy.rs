use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tracing::{error, info, warn};

use super::auth_client::AuthClient;
use super::db::RequestDb;
use crate::vm::VmRegistry;

const DNS_LISTEN_ADDR: &str = "0.0.0.0:10053";
const UPSTREAM_DNS: &str = "8.8.8.8:53";

/// Start the DNS proxy. Runs until cancel is triggered.
pub async fn run(
    registry: Arc<VmRegistry>,
    db: RequestDb,
    auth: Arc<AuthClient>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    ready: tokio::sync::oneshot::Sender<()>,
) {
    match run_inner(registry, db, auth, &mut cancel, ready).await {
        Ok(()) => info!("DNS proxy shut down"),
        Err(e) => error!("DNS proxy failed: {:#}", e),
    }
}

async fn run_inner(
    registry: Arc<VmRegistry>,
    db: RequestDb,
    auth: Arc<AuthClient>,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    ready: tokio::sync::oneshot::Sender<()>,
) -> Result<()> {
    let udp_socket = UdpSocket::bind(DNS_LISTEN_ADDR)
        .await
        .with_context(|| format!("Failed to bind DNS proxy UDP on {}", DNS_LISTEN_ADDR))?;

    let tcp_listener = TcpListener::bind(DNS_LISTEN_ADDR)
        .await
        .with_context(|| format!("Failed to bind DNS proxy TCP on {}", DNS_LISTEN_ADDR))?;

    info!("DNS proxy listening on {} (UDP+TCP)", DNS_LISTEN_ADDR);

    // Signal readiness now that both sockets are bound
    let _ = ready.send(());

    let udp_socket = Arc::new(udp_socket);
    let mut buf = vec![0u8; 4096];

    loop {
        tokio::select! {
            result = udp_socket.recv_from(&mut buf) => {
                let (len, peer_addr) = result.context("Failed to receive DNS packet")?;
                let packet = buf[..len].to_vec();

                let registry = registry.clone();
                let db = db.clone();
                let auth = auth.clone();
                let reply_socket = udp_socket.clone();

                // Spawn handler so we don't block the listener
                let upstream_socket = UdpSocket::bind("0.0.0.0:0").await;
                if let Ok(upstream_socket) = upstream_socket {
                    tokio::spawn(async move {
                        match process_dns_query(&packet, peer_addr, &registry, &db, &auth, &upstream_socket).await {
                            Ok(response) => {
                                if let Err(e) = reply_socket.send_to(&response, peer_addr).await {
                                    warn!("Failed to send DNS response to {}: {}", peer_addr, e);
                                }
                            }
                            Err(e) => {
                                warn!("DNS query from {} failed: {:#}", peer_addr, e);
                            }
                        }
                    });
                }
            }
            result = tcp_listener.accept() => {
                let (stream, peer_addr) = result.context("Failed to accept TCP DNS connection")?;

                let registry = registry.clone();
                let db = db.clone();
                let auth = auth.clone();

                tokio::spawn(async move {
                    if let Err(e) = handle_tcp_dns_connection(stream, peer_addr, &registry, &db, &auth).await {
                        warn!("TCP DNS connection from {} failed: {:#}", peer_addr, e);
                    }
                });
            }
            _ = cancel.changed() => {
                info!("DNS proxy received shutdown signal");
                break;
            }
        }
    }

    Ok(())
}

/// Process a single DNS query and return the response bytes.
/// Shared by both UDP and TCP handlers.
async fn process_dns_query(
    packet: &[u8],
    peer_addr: SocketAddr,
    registry: &VmRegistry,
    db: &RequestDb,
    auth: &AuthClient,
    upstream_socket: &UdpSocket,
) -> Result<Vec<u8>> {
    let start = Instant::now();

    // 1. Resolve vm_id
    let vm_id = registry
        .find_by_ip(peer_addr.ip())
        .await
        .map(|id| id.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // 2. Parse DNS query
    let (query_name, query_type) = parse_dns_question(packet)
        .unwrap_or(("unknown".to_string(), "unknown".to_string()));

    // 3. Log request
    let request_id = db
        .log_request(
            &vm_id, "dns", None, None, None,
            Some(&query_name), Some(&query_type),
            None, None, None,
        )
        .unwrap_or(0);

    // 4. Authorize
    let auth_start = Instant::now();
    let (allowed, reason) = auth
        .authorize_dns(request_id, &vm_id, &query_name, &query_type)
        .await
        .unwrap_or((false, "auth error".to_string()));
    let auth_latency = auth_start.elapsed().as_millis() as i64;

    if request_id > 0 {
        let _ = db.log_authorization(request_id, allowed, &reason, auth_latency);
    }

    // 5. If denied, respond with REFUSED
    if !allowed {
        let refused = build_refused_response(packet);
        let duration_ms = start.elapsed().as_millis() as i64;
        if request_id > 0 {
            let _ = db.log_response(request_id, Some(5), None, None, None, None, Some("REFUSED"), duration_ms);
        }
        return Ok(refused);
    }

    // 6. Forward to upstream via UDP
    upstream_socket
        .send_to(packet, UPSTREAM_DNS)
        .await
        .context("Failed to send to upstream DNS")?;

    let mut resp_buf = vec![0u8; 4096];
    let resp_len = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        upstream_socket.recv(&mut resp_buf),
    )
    .await
    .context("DNS upstream timeout")?
    .context("Failed to receive DNS response")?;

    let response = resp_buf[..resp_len].to_vec();

    // 7. Log response
    let duration_ms = start.elapsed().as_millis() as i64;
    let rcode = if resp_len >= 4 {
        Some((resp_buf[3] & 0x0F) as i32)
    } else {
        None
    };

    if request_id > 0 {
        let _ = db.log_response(request_id, rcode, Some(resp_len as i64), None, None, None, None, duration_ms);
    }

    Ok(response)
}

/// Handle a single TCP DNS connection. Reads length-prefixed messages in a loop.
async fn handle_tcp_dns_connection(
    mut stream: TcpStream,
    peer_addr: SocketAddr,
    registry: &VmRegistry,
    db: &RequestDb,
    auth: &AuthClient,
) -> Result<()> {
    loop {
        // Read 2-byte length prefix
        let msg_len = match read_dns_length(&mut stream).await {
            Ok(len) => len,
            Err(e) => {
                // EOF is normal — client closed connection
                if e.downcast_ref::<std::io::Error>()
                    .map_or(false, |io_err| io_err.kind() == std::io::ErrorKind::UnexpectedEof)
                {
                    break;
                }
                return Err(e);
            }
        };

        if msg_len == 0 {
            continue;
        }

        // Read the DNS message
        let mut msg_buf = vec![0u8; msg_len];
        stream
            .read_exact(&mut msg_buf)
            .await
            .context("Failed to read TCP DNS message")?;

        // Process via UDP upstream (standard for DNS proxies)
        let upstream_socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .context("Failed to bind upstream UDP socket for TCP DNS")?;

        let response = process_dns_query(&msg_buf, peer_addr, registry, db, auth, &upstream_socket).await?;

        // Write length-prefixed response
        write_dns_message(&mut stream, &response).await?;
    }

    Ok(())
}

/// Read a 2-byte big-endian DNS TCP length prefix.
async fn read_dns_length<R: AsyncRead + Unpin>(reader: &mut R) -> Result<usize> {
    let mut len_buf = [0u8; 2];
    reader.read_exact(&mut len_buf).await.context("Failed to read DNS TCP length prefix")?;
    Ok(u16::from_be_bytes(len_buf) as usize)
}

/// Write a DNS message with a 2-byte big-endian length prefix.
async fn write_dns_message<W: AsyncWrite + Unpin>(writer: &mut W, message: &[u8]) -> Result<()> {
    let len = (message.len() as u16).to_be_bytes();
    writer.write_all(&len).await.context("Failed to write DNS TCP length prefix")?;
    writer.write_all(message).await.context("Failed to write DNS TCP message")?;
    writer.flush().await.context("Failed to flush DNS TCP message")?;
    Ok(())
}

/// Parse the question section of a DNS query to extract name and type.
fn parse_dns_question(packet: &[u8]) -> Option<(String, String)> {
    if packet.len() < 12 {
        return None; // Too short for DNS header
    }

    // Question count (bytes 4-5)
    let qdcount = u16::from_be_bytes([packet[4], packet[5]]);
    if qdcount == 0 {
        return None;
    }

    // Parse question name starting at byte 12
    let mut pos = 12;
    let mut labels = Vec::new();

    loop {
        if pos >= packet.len() {
            return None;
        }
        let label_len = packet[pos] as usize;
        if label_len == 0 {
            pos += 1;
            break;
        }
        pos += 1;
        if pos + label_len > packet.len() {
            return None;
        }
        labels.push(String::from_utf8_lossy(&packet[pos..pos + label_len]).to_string());
        pos += label_len;
    }

    let name = labels.join(".");

    // Query type (2 bytes after name)
    if pos + 2 > packet.len() {
        return Some((name, "unknown".to_string()));
    }
    let qtype = u16::from_be_bytes([packet[pos], packet[pos + 1]]);
    let qtype_str = match qtype {
        1 => "A",
        2 => "NS",
        5 => "CNAME",
        6 => "SOA",
        15 => "MX",
        16 => "TXT",
        28 => "AAAA",
        33 => "SRV",
        255 => "ANY",
        _ => "OTHER",
    };

    Some((name, qtype_str.to_string()))
}

/// Build a REFUSED DNS response from a query packet.
fn build_refused_response(query: &[u8]) -> Vec<u8> {
    if query.len() < 12 {
        return vec![];
    }
    let mut resp = query.to_vec();
    // Set QR bit (response) and RCODE=5 (REFUSED)
    resp[2] = (resp[2] | 0x80) & 0xFD; // Set QR=1, clear AA
    resp[3] = (resp[3] & 0xF0) | 0x05; // RCODE=5
    // Zero out answer/authority/additional counts
    resp[6..12].copy_from_slice(&[0, 0, 0, 0, 0, 0]);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_dns_question() {
        // Minimal DNS query for "example.com" type A
        let mut packet = vec![
            0x00, 0x01, // ID
            0x01, 0x00, // Flags (standard query)
            0x00, 0x01, // QDCOUNT=1
            0x00, 0x00, // ANCOUNT=0
            0x00, 0x00, // NSCOUNT=0
            0x00, 0x00, // ARCOUNT=0
        ];
        // Question: example.com
        packet.extend_from_slice(&[7]); // "example" length
        packet.extend_from_slice(b"example");
        packet.extend_from_slice(&[3]); // "com" length
        packet.extend_from_slice(b"com");
        packet.push(0); // Root label
        packet.extend_from_slice(&[0x00, 0x01]); // QTYPE=A
        packet.extend_from_slice(&[0x00, 0x01]); // QCLASS=IN

        let (name, qtype) = parse_dns_question(&packet).unwrap();
        assert_eq!(name, "example.com");
        assert_eq!(qtype, "A");
    }

    #[test]
    fn test_build_refused_response() {
        let query = vec![
            0xAB, 0xCD, // ID
            0x01, 0x00, // Flags
            0x00, 0x01, // QDCOUNT
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let resp = build_refused_response(&query);
        assert_eq!(resp[0..2], [0xAB, 0xCD]); // ID preserved
        assert!(resp[2] & 0x80 != 0); // QR=1
        assert_eq!(resp[3] & 0x0F, 5); // RCODE=5
    }

    #[tokio::test]
    async fn test_tcp_dns_roundtrip() {
        let (mut client, mut server) = tokio::io::duplex(1024);

        let message = b"hello dns world";

        // Write a length-prefixed message from the client side
        write_dns_message(&mut client, message).await.unwrap();

        // Read it back from the server side
        let len = read_dns_length(&mut server).await.unwrap();
        assert_eq!(len, message.len());

        let mut buf = vec![0u8; len];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, message);
    }

    #[tokio::test]
    async fn test_tcp_dns_length_encoding() {
        // Test various sizes: 0, 512, 65535
        for &size in &[0usize, 512, 65535] {
            let (mut writer, mut reader) = tokio::io::duplex(4);

            let msg = vec![0xAA; size];
            let len_bytes = (size as u16).to_be_bytes();

            // Write raw length prefix
            writer.write_all(&len_bytes).await.unwrap();

            // Read it via our helper
            let read_len = read_dns_length(&mut reader).await.unwrap();
            assert_eq!(read_len, size, "Length mismatch for size {}", size);

            // Verify the write helper produces the same encoding
            let (mut w2, mut r2) = tokio::io::duplex(size + 4);
            write_dns_message(&mut w2, &msg).await.unwrap();

            let decoded_len = read_dns_length(&mut r2).await.unwrap();
            assert_eq!(decoded_len, size, "Write helper length mismatch for size {}", size);

            if size > 0 {
                let mut decoded_buf = vec![0u8; decoded_len];
                r2.read_exact(&mut decoded_buf).await.unwrap();
                assert_eq!(decoded_buf, msg);
            }
        }
    }
}
