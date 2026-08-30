use crate::error::{OnionError, Result};
use std::fmt;

/// An anonymous network address — either a Tor .onion or an I2P .i2p destination.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OnionAddr {
    /// A Tor v3 hidden service address (56-char base32 + ".onion").
    TorV3 { host: String, port: u16 },
    /// A Tor v2 hidden service address (16-char base32 + ".onion") — legacy.
    TorV2 { host: String, port: u16 },
    /// An I2P destination address (base32 + ".b32.i2p" or full base64 dest).
    I2p { dest: String, port: u16 },
}

impl OnionAddr {
    /// Parse an address string into an OnionAddr.
    ///
    /// Accepts:
    /// - `"abc...xyz.onion:22526"` — Tor hidden service
    /// - `"abc...xyz.b32.i2p:22526"` — I2P base32 address
    pub fn parse(addr: &str) -> Result<Self> {
        // Split host:port
        let (host, port_str) = if let Some(colon) = addr.rfind(':') {
            (&addr[..colon], &addr[colon + 1..])
        } else {
            return Err(OnionError::InvalidOnionAddr(format!(
                "No port in address: {}",
                addr
            )));
        };

        let port: u16 = port_str
            .parse()
            .map_err(|_| OnionError::InvalidOnionAddr(format!("Invalid port: {}", port_str)))?;

        let host_lower = host.to_lowercase();

        if host_lower.ends_with(".onion") {
            let label = host_lower.trim_end_matches(".onion");
            if label.len() == 56 {
                Ok(OnionAddr::TorV3 {
                    host: host_lower,
                    port,
                })
            } else if label.len() == 16 {
                Ok(OnionAddr::TorV2 {
                    host: host_lower,
                    port,
                })
            } else {
                Err(OnionError::InvalidOnionAddr(format!(
                    "Invalid .onion address length {}: {}",
                    label.len(),
                    addr
                )))
            }
        } else if host_lower.ends_with(".i2p") || host_lower.ends_with(".b32.i2p") {
            // The destination is interpolated into SAM protocol commands;
            // restrict it to base32/base64 charset so a peer-supplied address
            // cannot inject newlines or spaces into the SAM session.
            let label = host_lower.trim_end_matches(".i2p");
            if label.is_empty()
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'=' || b == b'.')
            {
                return Err(OnionError::InvalidOnionAddr(format!(
                    "Invalid characters in I2P destination: {}",
                    addr
                )));
            }
            Ok(OnionAddr::I2p {
                dest: host_lower,
                port,
            })
        } else {
            Err(OnionError::InvalidOnionAddr(format!(
                "Not a .onion or .i2p address: {}",
                addr
            )))
        }
    }

    /// Returns true if this is a Tor address.
    pub fn is_tor(&self) -> bool {
        matches!(self, OnionAddr::TorV3 { .. } | OnionAddr::TorV2 { .. })
    }

    /// Returns true if this is an I2P address.
    pub fn is_i2p(&self) -> bool {
        matches!(self, OnionAddr::I2p { .. })
    }

    /// Returns the host string (e.g., `"abc.onion"`).
    pub fn host(&self) -> &str {
        match self {
            OnionAddr::TorV3 { host, .. } => host,
            OnionAddr::TorV2 { host, .. } => host,
            OnionAddr::I2p { dest, .. } => dest,
        }
    }

    /// Returns the port.
    pub fn port(&self) -> u16 {
        match self {
            OnionAddr::TorV3 { port, .. } => *port,
            OnionAddr::TorV2 { port, .. } => *port,
            OnionAddr::I2p { port, .. } => *port,
        }
    }

    /// Returns the full address string (host:port).
    pub fn to_addr_string(&self) -> String {
        format!("{}:{}", self.host(), self.port())
    }
}

impl fmt::Display for OnionAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_addr_string())
    }
}

/// Returns true if the given address string is a .onion or .i2p address.
pub fn is_anon_addr(addr: &str) -> bool {
    let lower = addr.to_lowercase();
    lower.contains(".onion") || lower.contains(".i2p")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tor_v3() {
        let addr = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion:22526";
        let parsed = OnionAddr::parse(addr).unwrap();
        assert!(parsed.is_tor());
        assert_eq!(parsed.port(), 22526);
    }

    #[test]
    fn test_parse_tor_v2() {
        let addr = "3g2upl4pq6kufc4m.onion:22526";
        let parsed = OnionAddr::parse(addr).unwrap();
        assert!(matches!(parsed, OnionAddr::TorV2 { .. }));
    }

    #[test]
    fn test_parse_i2p() {
        let addr = "zzz.i2p:22526";
        let parsed = OnionAddr::parse(addr).unwrap();
        assert!(parsed.is_i2p());
        assert_eq!(parsed.port(), 22526);
    }

    /// The I2P destination is interpolated into SAM protocol commands —
    /// addresses containing newlines, spaces, or control characters must be
    /// rejected so a peer-supplied value cannot inject SAM commands.
    #[test]
    fn test_parse_i2p_rejects_injection_characters() {
        assert!(OnionAddr::parse("abc\nSTREAM X.i2p:1").is_err());
        assert!(OnionAddr::parse("abc def.i2p:1").is_err());
        assert!(OnionAddr::parse("abc\r\n.i2p:1").is_err());
        // Legitimate base32 still parses.
        assert!(OnionAddr::parse("abc123.b32.i2p:1").is_ok());
    }

    #[test]
    fn test_parse_invalid() {
        assert!(OnionAddr::parse("example.com:22526").is_err());
        assert!(OnionAddr::parse("noport.onion").is_err());
    }

    #[test]
    fn test_is_anon_addr() {
        assert!(is_anon_addr("abc.onion:22526"));
        assert!(is_anon_addr("abc.b32.i2p:22526"));
        assert!(!is_anon_addr("192.168.1.1:22526"));
    }
}
