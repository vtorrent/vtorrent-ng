use serde::{Deserialize, Serialize};

/// Information about a created hidden service (Tor or I2P).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiddenServiceInfo {
    /// The public address (e.g., `"abc...xyz.onion:22526"` or `"abc.b32.i2p:22526"`).
    pub onion_addr: String,
    /// The private key for the hidden service (None if ephemeral/discarded).
    pub private_key: Option<String>,
    /// The local port the hidden service forwards to.
    pub local_port: u16,
    /// The virtual port exposed on the hidden service.
    pub virtual_port: u16,
}

impl HiddenServiceInfo {
    /// Returns true if this is a Tor hidden service.
    pub fn is_tor(&self) -> bool {
        self.onion_addr.contains(".onion")
    }

    /// Returns true if this is an I2P destination.
    pub fn is_i2p(&self) -> bool {
        self.onion_addr.contains(".i2p")
    }

    /// Returns the host portion of the address (without port).
    pub fn host(&self) -> &str {
        self.onion_addr.split(':').next().unwrap_or(&self.onion_addr)
    }
}
