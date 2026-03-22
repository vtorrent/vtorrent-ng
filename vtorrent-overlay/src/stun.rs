/// STUN-based external IP/port discovery.
///
/// Implements a minimal subset of RFC 5389 (STUN) sufficient to discover
/// the node's external (public) UDP socket address from behind NAT.
///
/// We query multiple STUN servers in parallel and take the first response.
/// No STUN library dependency — the binding request/response is ~20 bytes.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::timeout;

use crate::error::{OverlayError, Result};

/// Well-known free STUN servers (no account required).
pub const STUN_SERVERS: &[&str] = &[
    "stun.l.google.com:19302",
    "stun1.l.google.com:19302",
    "stun.cloudflare.com:3478",
    "stun.stunprotocol.org:3478",
    "stun.voip.blackberry.com:3478",
];

/// STUN message type: Binding Request
const BINDING_REQUEST: u16 = 0x0001;
/// STUN magic cookie (RFC 5389)
const MAGIC_COOKIE: u32 = 0x2112A442;
/// STUN XOR-MAPPED-ADDRESS attribute type
const XOR_MAPPED_ADDRESS: u16 = 0x0020;
/// STUN MAPPED-ADDRESS attribute type (RFC 3489 compat)
const MAPPED_ADDRESS: u16 = 0x0001;

/// Discover the node's external UDP address by querying STUN servers.
///
/// Binds to `bind_addr` (e.g. `0.0.0.0:0`), queries all STUN servers in
/// parallel, and returns the first successful external address.
pub async fn discover_external_addr(bind_addr: &str) -> Result<SocketAddr> {
    let socket = UdpSocket::bind(bind_addr)
        .await
        .map_err(OverlayError::Io)?;
    let socket = std::sync::Arc::new(socket);

    // Try all STUN servers concurrently; return first success
    let mut handles = Vec::new();
    for server in STUN_SERVERS {
        let sock = socket.clone();
        let server = server.to_string();
        handles.push(tokio::spawn(async move {
            query_stun_server(&sock, &server).await
        }));
    }

    for handle in handles {
        if let Ok(Ok(addr)) = handle.await {
            return Ok(addr);
        }
    }

    Err(OverlayError::Stun(
        "all STUN servers unreachable".to_string(),
    ))
}

/// Query a single STUN server and return the reflected external address.
async fn query_stun_server(socket: &UdpSocket, server: &str) -> Result<SocketAddr> {
    // Resolve the server address
    let server_addr: SocketAddr = tokio::net::lookup_host(server)
        .await
        .map_err(|e| OverlayError::Stun(e.to_string()))?
        .next()
        .ok_or_else(|| OverlayError::Stun(format!("could not resolve {}", server)))?;

    // Build a STUN Binding Request (20 bytes)
    let transaction_id = rand::random::<[u8; 12]>();
    let request = build_binding_request(&transaction_id);

    // Send the request
    socket
        .send_to(&request, server_addr)
        .await
        .map_err(OverlayError::Io)?;

    // Wait for response (500ms timeout per server)
    let mut buf = [0u8; 512];
    let (n, _from) = timeout(Duration::from_millis(500), socket.recv_from(&mut buf))
        .await
        .map_err(|_| OverlayError::Timeout)?
        .map_err(OverlayError::Io)?;

    parse_binding_response(&buf[..n], &transaction_id)
}

/// Build a minimal STUN Binding Request message.
fn build_binding_request(transaction_id: &[u8; 12]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(20);
    // Message type: Binding Request
    msg.extend_from_slice(&BINDING_REQUEST.to_be_bytes());
    // Message length: 0 (no attributes)
    msg.extend_from_slice(&0u16.to_be_bytes());
    // Magic cookie
    msg.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
    // Transaction ID (12 bytes)
    msg.extend_from_slice(transaction_id);
    msg
}

/// Parse a STUN Binding Response and extract the external address.
fn parse_binding_response(data: &[u8], expected_txid: &[u8; 12]) -> Result<SocketAddr> {
    if data.len() < 20 {
        return Err(OverlayError::Stun("response too short".into()));
    }

    // Verify magic cookie
    let cookie = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    if cookie != MAGIC_COOKIE {
        return Err(OverlayError::Stun("bad magic cookie".into()));
    }

    // Verify transaction ID
    if &data[8..20] != expected_txid {
        return Err(OverlayError::Stun("transaction ID mismatch".into()));
    }

    // Parse attributes
    let msg_len = u16::from_be_bytes([data[2], data[3]]) as usize;
    let mut pos = 20usize;
    let end = (20 + msg_len).min(data.len());

    while pos + 4 <= end {
        let attr_type = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let attr_len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;

        if pos + attr_len > end {
            break;
        }

        match attr_type {
            XOR_MAPPED_ADDRESS => {
                return parse_xor_mapped_address(&data[pos..pos + attr_len]);
            }
            MAPPED_ADDRESS => {
                return parse_mapped_address(&data[pos..pos + attr_len]);
            }
            _ => {}
        }

        // Attributes are padded to 4-byte boundaries
        pos += (attr_len + 3) & !3;
    }

    Err(OverlayError::Stun("no mapped address in response".into()))
}

/// Parse a STUN XOR-MAPPED-ADDRESS attribute.
fn parse_xor_mapped_address(data: &[u8]) -> Result<SocketAddr> {
    if data.len() < 8 {
        return Err(OverlayError::Stun("XOR-MAPPED-ADDRESS too short".into()));
    }
    let family = data[1];
    let xport = u16::from_be_bytes([data[2], data[3]]);
    let port = xport ^ (MAGIC_COOKIE >> 16) as u16;

    match family {
        0x01 => {
            // IPv4
            if data.len() < 8 {
                return Err(OverlayError::Stun("XOR-MAPPED-ADDRESS IPv4 too short".into()));
            }
            let xip = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
            let ip = xip ^ MAGIC_COOKIE;
            let addr = std::net::Ipv4Addr::from(ip);
            Ok(SocketAddr::new(std::net::IpAddr::V4(addr), port))
        }
        _ => Err(OverlayError::Stun("only IPv4 supported".into())),
    }
}

/// Parse a STUN MAPPED-ADDRESS attribute (RFC 3489, no XOR).
fn parse_mapped_address(data: &[u8]) -> Result<SocketAddr> {
    if data.len() < 8 {
        return Err(OverlayError::Stun("MAPPED-ADDRESS too short".into()));
    }
    let family = data[1];
    let port = u16::from_be_bytes([data[2], data[3]]);
    match family {
        0x01 => {
            let ip = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
            let addr = std::net::Ipv4Addr::from(ip);
            Ok(SocketAddr::new(std::net::IpAddr::V4(addr), port))
        }
        _ => Err(OverlayError::Stun("only IPv4 supported".into())),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_binding_request_length() {
        let txid = [0u8; 12];
        let req = build_binding_request(&txid);
        assert_eq!(req.len(), 20);
        // Message type
        assert_eq!(u16::from_be_bytes([req[0], req[1]]), BINDING_REQUEST);
        // Message length (no attributes)
        assert_eq!(u16::from_be_bytes([req[2], req[3]]), 0);
        // Magic cookie
        assert_eq!(u32::from_be_bytes([req[4], req[5], req[6], req[7]]), MAGIC_COOKIE);
    }

    #[test]
    fn test_parse_xor_mapped_address_ipv4() {
        // XOR-encode 1.2.3.4:22526
        let ip: u32 = u32::from(std::net::Ipv4Addr::new(1, 2, 3, 4));
        let port: u16 = 22526;
        let xip = ip ^ MAGIC_COOKIE;
        let xport = port ^ (MAGIC_COOKIE >> 16) as u16;

        let mut attr = vec![0u8; 8];
        attr[1] = 0x01; // IPv4
        attr[2..4].copy_from_slice(&xport.to_be_bytes());
        attr[4..8].copy_from_slice(&xip.to_be_bytes());

        let addr = parse_xor_mapped_address(&attr).unwrap();
        assert_eq!(addr.port(), 22526);
        assert_eq!(addr.ip().to_string(), "1.2.3.4");
    }

    #[test]
    fn test_parse_binding_response_bad_cookie() {
        let mut data = vec![0u8; 20];
        data[4..8].copy_from_slice(&0xDEADBEEFu32.to_be_bytes());
        let txid = [0u8; 12];
        assert!(parse_binding_response(&data, &txid).is_err());
    }
}
