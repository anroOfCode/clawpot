use anyhow::{Context, Result};
use std::net::{IpAddr, SocketAddr};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Write a PROXY protocol v1 header for a TCP4 connection.
///
/// Format: `PROXY TCP4 <src_ip> <dst_ip> <src_port> <dst_port>\r\n`
///
/// This is sent by the TLS MITM proxy to the HTTP proxy so the HTTP proxy
/// knows the real client IP (instead of seeing 127.0.0.1).
pub async fn write_proxy_header<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    client_addr: SocketAddr,
    server_addr: SocketAddr,
) -> Result<()> {
    let line = format!(
        "PROXY TCP4 {} {} {} {}\r\n",
        client_addr.ip(),
        server_addr.ip(),
        client_addr.port(),
        server_addr.port(),
    );
    writer
        .write_all(line.as_bytes())
        .await
        .context("Failed to write PROXY protocol header")?;
    Ok(())
}

/// Read a PROXY protocol v1 header and return the original client IP.
///
/// Reads byte-by-byte until `\n`, so it works directly on a `TcpStream`
/// without needing a `BufReader` (which would conflict with hyper's need
/// for `AsyncWrite` on the same stream).
pub async fn read_proxy_header<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<IpAddr> {
    let mut line = Vec::with_capacity(108);
    loop {
        let mut byte = [0u8; 1];
        reader
            .read_exact(&mut byte)
            .await
            .context("Failed to read PROXY protocol header")?;
        line.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
        anyhow::ensure!(line.len() <= 256, "PROXY protocol header too long");
    }
    parse_proxy_line(&line)
}

fn parse_proxy_line(line: &[u8]) -> Result<IpAddr> {
    let text = std::str::from_utf8(line).context("PROXY header is not valid UTF-8")?;
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.len() != 6 || parts[0] != "PROXY" {
        anyhow::bail!("Invalid PROXY protocol header: {}", text.trim());
    }
    parts[2]
        .parse()
        .context("Invalid source IP in PROXY header")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_round_trip() {
        let client: SocketAddr = "192.168.100.2:45678".parse().unwrap();
        let server: SocketAddr = "0.0.0.0:10443".parse().unwrap();

        let mut buf = Vec::new();
        write_proxy_header(&mut buf, client, server).await.unwrap();

        let expected = "PROXY TCP4 192.168.100.2 0.0.0.0 45678 10443\r\n";
        assert_eq!(String::from_utf8_lossy(&buf), expected);

        let ip = read_proxy_header(&mut &buf[..]).await.unwrap();
        assert_eq!(ip, client.ip());
    }

    #[tokio::test]
    async fn test_invalid_header() {
        let data = b"GET / HTTP/1.1\r\n";
        let result = read_proxy_header(&mut &data[..]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_invalid_ip() {
        let data = b"PROXY TCP4 not-an-ip 0.0.0.0 1234 5678\r\n";
        let result = read_proxy_header(&mut &data[..]).await;
        assert!(result.is_err());
    }
}
