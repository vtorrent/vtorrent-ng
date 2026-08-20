use crate::{
    config::TransportConfig,
    error::{OnionError, Result},
    hidden_service::HiddenServiceInfo,
};
/// Tor transport implementation.
///
/// Connects to the Tor SOCKS5 proxy (default: 127.0.0.1:9050) to route
/// TCP connections through the Tor network. Supports both .onion addresses
/// and clearnet addresses (when `prefer_onion` is enabled).
///
/// Also provides a control port interface for:
/// - Checking Tor bootstrap status
/// - Creating ephemeral hidden services (ADD_ONION)
/// - Requesting new circuits (SIGNAL NEWNYM)
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Tor transport — wraps SOCKS5 and control port interactions.
pub struct TorTransport {
    config: TransportConfig,
}

impl TorTransport {
    pub fn new(config: TransportConfig) -> Self {
        Self { config }
    }

    /// Check if the Tor SOCKS5 proxy is reachable.
    pub async fn is_available(&self) -> bool {
        let timeout = Duration::from_secs(3);
        tokio::time::timeout(timeout, TcpStream::connect(&self.config.tor_socks_addr))
            .await
            .is_ok_and(|r| r.is_ok())
    }

    /// Connect to `target_addr` through the Tor SOCKS5 proxy.
    ///
    /// `target_addr` can be:
    /// - A `.onion:port` address (routed to a hidden service)
    /// - A `hostname:port` or `ip:port` (routed through Tor exit node)
    pub async fn connect(&self, target_addr: &str) -> Result<TcpStream> {
        let timeout = Duration::from_secs(self.config.connect_timeout_secs);

        // Parse target host and port
        let (host, port) = split_host_port(target_addr)?;

        // Connect to SOCKS5 proxy
        let mut proxy =
            tokio::time::timeout(timeout, TcpStream::connect(&self.config.tor_socks_addr))
                .await
                .map_err(|_| OnionError::Timeout(self.config.connect_timeout_secs))?
                .map_err(|e| OnionError::TorUnavailable {
                    addr: self.config.tor_socks_addr.clone(),
                    source: e,
                })?;

        // SOCKS5 handshake
        socks5_connect(&mut proxy, &host, port).await?;

        Ok(proxy)
    }

    /// Get the Tor bootstrap status via the control port.
    /// Returns a percentage (0–100) or None if control port is unavailable.
    pub async fn bootstrap_status(&self) -> Option<u8> {
        let mut stream = TcpStream::connect(&self.config.tor_control_addr)
            .await
            .ok()?;

        // Authenticate
        let auth_cmd = if self.config.tor_control_password.is_empty() {
            "AUTHENTICATE\r\n".to_string()
        } else {
            format!("AUTHENTICATE \"{}\"\r\n", self.config.tor_control_password)
        };

        stream.write_all(auth_cmd.as_bytes()).await.ok()?;
        let mut buf = [0u8; 256];
        let n = stream.read(&mut buf).await.ok()?;
        let resp = std::str::from_utf8(&buf[..n]).ok()?;
        if !resp.starts_with("250") {
            return None;
        }

        // Query bootstrap status
        stream
            .write_all(b"GETINFO status/bootstrap-phase\r\n")
            .await
            .ok()?;
        let n = stream.read(&mut buf).await.ok()?;
        let resp = std::str::from_utf8(&buf[..n]).ok()?;

        // Parse "PROGRESS=XX" from the response
        resp.split_whitespace()
            .find(|s| s.starts_with("PROGRESS="))
            .and_then(|s| s.strip_prefix("PROGRESS="))
            .and_then(|s| s.parse::<u8>().ok())
    }

    /// Create an ephemeral Tor hidden service via the control port.
    ///
    /// Returns the .onion address and the private key (for persistence).
    /// Uses ED25519-V3 (v3 onion addresses) by default.
    pub async fn create_hidden_service(
        &self,
        local_port: u16,
        virtual_port: u16,
    ) -> Result<HiddenServiceInfo> {
        let mut stream = TcpStream::connect(&self.config.tor_control_addr)
            .await
            .map_err(|e| {
                OnionError::HiddenServiceError(format!("Control port unavailable: {}", e))
            })?;

        // Authenticate
        let auth_cmd = if self.config.tor_control_password.is_empty() {
            "AUTHENTICATE\r\n".to_string()
        } else {
            format!("AUTHENTICATE \"{}\"\r\n", self.config.tor_control_password)
        };
        stream.write_all(auth_cmd.as_bytes()).await?;
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await?;
        let resp = std::str::from_utf8(&buf[..n]).unwrap_or("");
        if !resp.starts_with("250") {
            return Err(OnionError::HiddenServiceError(format!(
                "Auth failed: {}",
                resp.trim()
            )));
        }

        // ADD_ONION command — creates an ephemeral hidden service
        let cmd = format!(
            "ADD_ONION NEW:ED25519-V3 Port={},{} Flags=DiscardPK\r\n",
            virtual_port, local_port
        );
        stream.write_all(cmd.as_bytes()).await?;
        let n = stream.read(&mut buf).await?;
        let resp = std::str::from_utf8(&buf[..n]).unwrap_or("").to_string();

        if !resp.contains("250-ServiceID=") {
            return Err(OnionError::HiddenServiceError(format!(
                "ADD_ONION failed: {}",
                resp.trim()
            )));
        }

        // Parse ServiceID
        let service_id = resp
            .lines()
            .find(|l| l.starts_with("250-ServiceID="))
            .and_then(|l| l.strip_prefix("250-ServiceID="))
            .ok_or_else(|| {
                OnionError::HiddenServiceError("Missing ServiceID in response".to_string())
            })?
            .trim()
            .to_string();

        let onion_addr = format!("{}.onion:{}", service_id, virtual_port);

        tracing::info!("Created Tor hidden service: {}", onion_addr);

        Ok(HiddenServiceInfo {
            onion_addr,
            private_key: None, // DiscardPK flag used — ephemeral
            local_port,
            virtual_port,
        })
    }

    /// Request a new Tor circuit (SIGNAL NEWNYM).
    /// This changes the exit node used for subsequent connections.
    pub async fn new_circuit(&self) -> Result<()> {
        let mut stream = TcpStream::connect(&self.config.tor_control_addr)
            .await
            .map_err(|e| {
                OnionError::HiddenServiceError(format!("Control port unavailable: {}", e))
            })?;

        let auth_cmd = if self.config.tor_control_password.is_empty() {
            "AUTHENTICATE\r\n".to_string()
        } else {
            format!("AUTHENTICATE \"{}\"\r\n", self.config.tor_control_password)
        };
        stream.write_all(auth_cmd.as_bytes()).await?;
        read_control_reply(&mut stream).await?;

        stream.write_all(b"SIGNAL NEWNYM\r\n").await?;
        read_control_reply(&mut stream).await?;

        Ok(())
    }
}

/// Read one bounded, line-oriented Tor control-port response.
async fn read_control_reply(stream: &mut TcpStream) -> Result<()> {
    const MAX_CONTROL_REPLY: usize = 4096;
    let mut response = Vec::new();
    loop {
        if response.len() >= MAX_CONTROL_REPLY {
            return Err(OnionError::HiddenServiceError(
                "Tor control response exceeded maximum length".into(),
            ));
        }
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await?;
        response.push(byte[0]);
        if response.ends_with(b"\r\n") {
            break;
        }
    }

    if !response.starts_with(b"250") {
        return Err(OnionError::HiddenServiceError(format!(
            "Tor control command failed: {}",
            String::from_utf8_lossy(&response).trim(),
        )));
    }
    Ok(())
}

/// Perform a SOCKS5 handshake and CONNECT request.
async fn socks5_connect(stream: &mut TcpStream, host: &str, port: u16) -> Result<()> {
    // ── Step 1: Greeting ─────────────────────────────────────────────────
    // Version 5, 1 auth method: no auth (0x00)
    stream.write_all(&[0x05, 0x01, 0x00]).await?;

    let mut buf = [0u8; 2];
    stream.read_exact(&mut buf).await?;
    if buf[0] != 0x05 || buf[1] != 0x00 {
        return Err(OnionError::Socks5Error(format!(
            "Unexpected greeting response: {:02x} {:02x}",
            buf[0], buf[1]
        )));
    }

    // ── Step 2: CONNECT request ───────────────────────────────────────────
    let host_bytes = host.as_bytes();
    let host_len = host_bytes.len() as u8;

    let mut req = vec![
        0x05,     // SOCKS version
        0x01,     // CONNECT command
        0x00,     // reserved
        0x03,     // address type: domain name
        host_len, // domain name length
    ];
    req.extend_from_slice(host_bytes);
    req.push((port >> 8) as u8);
    req.push((port & 0xff) as u8);

    stream.write_all(&req).await?;

    // ── Step 3: Read response ─────────────────────────────────────────────
    // The reply is: VER(1) REP(1) RSV(1) ATYP(1) BND.ADDR(variable) BND.PORT(2).
    // Read the fixed 4-byte header first, then the variable-length address
    // based on ATYP, so domain (0x03) and IPv6 (0x04) replies are handled
    // without leaving trailing bytes in the stream.
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;

    if head[0] != 0x05 {
        return Err(OnionError::Socks5Error("Not a SOCKS5 response".to_string()));
    }
    if head[1] != 0x00 {
        let reason = match head[1] {
            0x01 => "general failure",
            0x02 => "connection not allowed",
            0x03 => "network unreachable",
            0x04 => "host unreachable",
            0x05 => "connection refused",
            0x06 => "TTL expired",
            0x07 => "command not supported",
            0x08 => "address type not supported",
            _ => "unknown error",
        };
        return Err(OnionError::Socks5Error(format!(
            "SOCKS5 CONNECT failed: {} (code {})",
            reason, head[1]
        )));
    }

    // Consume the BND.ADDR based on its address type.
    match head[3] {
        0x01 => {
            // IPv4: 4 bytes.
            let mut addr = [0u8; 4];
            stream.read_exact(&mut addr).await?;
        }
        0x04 => {
            // IPv6: 16 bytes.
            let mut addr = [0u8; 16];
            stream.read_exact(&mut addr).await?;
        }
        0x03 => {
            // Domain: 1 length byte + that many bytes.
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut addr = vec![0u8; len[0] as usize];
            stream.read_exact(&mut addr).await?;
        }
        other => {
            return Err(OnionError::Socks5Error(format!(
                "Unsupported SOCKS5 address type: {}",
                other
            )));
        }
    }

    // Consume the BND.PORT (2 bytes).
    let mut port = [0u8; 2];
    stream.read_exact(&mut port).await?;

    Ok(())
}

/// Split "host:port" into (host, port).
fn split_host_port(addr: &str) -> Result<(String, u16)> {
    let colon = addr
        .rfind(':')
        .ok_or_else(|| OnionError::InvalidOnionAddr(format!("No port in address: {}", addr)))?;
    let host = addr[..colon].to_string();
    let port: u16 = addr[colon + 1..]
        .parse()
        .map_err(|_| OnionError::InvalidOnionAddr(format!("Invalid port in: {}", addr)))?;
    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TransportConfig;

    #[test]
    fn test_split_host_port() {
        let (host, port) = split_host_port("abc.onion:22526").unwrap();
        assert_eq!(host, "abc.onion");
        assert_eq!(port, 22526);
    }

    #[test]
    fn test_split_host_port_ipv4() {
        let (host, port) = split_host_port("192.168.1.1:8080").unwrap();
        assert_eq!(host, "192.168.1.1");
        assert_eq!(port, 8080);
    }

    #[test]
    fn test_split_host_port_no_port() {
        assert!(split_host_port("abc.onion").is_err());
    }

    #[tokio::test]
    async fn test_tor_unavailable() {
        // Tor is not running in CI — just verify graceful failure
        let config = TransportConfig {
            tor_socks_addr: "127.0.0.1:19050".to_string(), // wrong port
            ..Default::default()
        };
        let tor = TorTransport::new(config);
        assert!(!tor.is_available().await);
    }
}
