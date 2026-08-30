use crate::crypto::{checksum, hash160};
use crate::error::{CoreError, Result};
use crate::keys::serialize_pubkey;
use secp256k1::PublicKey;

/// A P2PKH address (Pay-to-Public-Key-Hash).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Address {
    /// The 20-byte public key hash.
    pub hash: [u8; 20],
    /// The version byte (determines the address prefix character).
    pub version: u8,
}

impl Address {
    /// Derive a P2PKH address from a public key.
    pub fn from_pubkey(pubkey: &PublicKey, compressed: bool, version: u8) -> Self {
        let pubkey_bytes = serialize_pubkey(pubkey, compressed);
        let hash = hash160(&pubkey_bytes);
        Self { hash, version }
    }

    /// Decode a Base58Check-encoded address string.
    pub fn parse(s: &str) -> Result<Self> {
        let decoded = bs58::decode(s)
            .into_vec()
            .map_err(|_| CoreError::InvalidAddress(s.to_string()))?;

        if decoded.len() != 25 {
            return Err(CoreError::InvalidAddress(format!(
                "Expected 25 bytes, got {}",
                decoded.len()
            )));
        }

        // Verify checksum
        let (payload, check) = decoded.split_at(21);
        let expected = checksum(payload);
        if check != expected {
            return Err(CoreError::ChecksumMismatch);
        }

        let version = payload[0];
        let mut hash = [0u8; 20];
        hash.copy_from_slice(&payload[1..21]);

        Ok(Self { hash, version })
    }

    /// Encode the address to a Base58Check string.
    fn encode(&self) -> String {
        let mut payload = Vec::with_capacity(21);
        payload.push(self.version);
        payload.extend_from_slice(&self.hash);
        let check = checksum(&payload);
        payload.extend_from_slice(&check);
        bs58::encode(payload).into_string()
    }

    /// Create an address directly from a 20-byte hash (used by the snapshot parser).
    ///
    /// Rejects any input that is not exactly 20 bytes rather than silently
    /// padding or truncating, which could produce a wrong address.
    pub fn from_hash160(hash: &[u8], version: u8) -> Result<Self> {
        let h: [u8; 20] = hash.try_into().map_err(|_| {
            CoreError::InvalidAddress(format!("Expected 20-byte hash160, got {}", hash.len()))
        })?;
        Ok(Self { hash: h, version })
    }

    /// Check if this address uses the legacy vTorrent version byte.
    pub fn is_legacy(&self) -> bool {
        self.version == crate::network::legacy::PUBKEY_ADDRESS_PREFIX
    }

    /// Build the standard 25-byte P2PKH scriptPubKey locking to this address:
    /// `OP_DUP OP_HASH160 <20-byte hash> OP_EQUALVERIFY OP_CHECKSIG`.
    pub fn p2pkh_script_pubkey(&self) -> Vec<u8> {
        let mut script = Vec::with_capacity(25);
        script.push(0x76); // OP_DUP
        script.push(0xa9); // OP_HASH160
        script.push(0x14); // push 20 bytes
        script.extend_from_slice(&self.hash);
        script.push(0x88); // OP_EQUALVERIFY
        script.push(0xac); // OP_CHECKSIG
        script
    }
}

impl std::str::FromStr for Address {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.encode())
    }
}

/// Build the standard 25-byte P2PKH scriptPubKey from a 20-byte hash160:
/// `OP_DUP OP_HASH160 <hash> OP_EQUALVERIFY OP_CHECKSIG`.
pub fn p2pkh_script_pubkey(hash160: &[u8; 20]) -> Vec<u8> {
    Address {
        hash: *hash160,
        version: 0,
    }
    .p2pkh_script_pubkey()
}

/// Validate that a Base58Check address decodes to a vTorrent P2PKH address
/// (version byte 70). Rejects foreign-network addresses whose funds would be
/// unrecoverable on the VTR chain.
pub fn validate_p2pkh(s: &str) -> Result<Address> {
    let addr = Address::parse(s)?;
    if addr.version != crate::network::legacy::PUBKEY_ADDRESS_PREFIX {
        return Err(crate::error::CoreError::InvalidAddress(format!(
            "not a vTorrent mainnet address (version byte {})",
            addr.version
        )));
    }
    Ok(addr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::legacy;

    #[test]
    fn test_address_roundtrip() {
        // Create a dummy 20-byte hash
        let hash = [
            0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23,
            0x45, 0x67, 0x89, 0xab, 0xcd, 0xef,
        ];
        let addr = Address {
            hash,
            version: legacy::PUBKEY_ADDRESS_PREFIX,
        };
        let encoded = addr.to_string();
        let decoded: Address = encoded.parse().expect("Decode failed");
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_address_starts_with_v() {
        // Legacy vTorrent addresses (version byte 70) should start with 'V'
        let hash = [0u8; 20];
        let addr = Address { hash, version: 70 };
        let encoded = addr.to_string();
        assert!(
            encoded.starts_with('V'),
            "Expected 'V' prefix, got: {}",
            encoded
        );
    }
}
