use crate::crypto::checksum;
use crate::error::{CoreError, Result};
use secp256k1::{PublicKey, Secp256k1, SecretKey};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// A private key with automatic zeroing of memory on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct PrivateKey {
    inner: [u8; 32],
    compressed: bool,
}

impl PrivateKey {
    /// Create a new private key from raw bytes.
    pub fn from_bytes(bytes: [u8; 32], compressed: bool) -> Result<Self> {
        // Validate the key is a valid secp256k1 scalar
        SecretKey::from_slice(&bytes).map_err(CoreError::Secp256k1)?;
        Ok(Self {
            inner: bytes,
            compressed,
        })
    }

    /// Decode a WIF-encoded private key.
    /// Supports both legacy vTorrent WIF prefix (version byte 198 → starts with '7')
    /// and standard WIF (version byte 128 → starts with '5', 'K', or 'L').
    pub fn from_wif(wif: &str) -> Result<Self> {
        let decoded = bs58::decode(wif)
            .into_vec()
            .map_err(|_| CoreError::InvalidWif(wif.to_string()))?;

        if decoded.len() < 33 {
            return Err(CoreError::InvalidWif("Too short".to_string()));
        }

        // Verify checksum
        let (payload, check) = decoded.split_at(decoded.len() - 4);
        let expected = checksum(payload);
        if check != expected {
            return Err(CoreError::ChecksumMismatch);
        }

        // payload[0] is the version byte (198 for legacy VTR, 128 for standard)
        // Reject any other version: a foreign-network WIF (e.g. Bitcoin
        // testnet 0xEF) must not silently import as a VTR key.
        match payload[0] {
            crate::network::legacy::SECRET_KEY_PREFIX | 128 => {}
            other => {
                return Err(CoreError::InvalidWif(format!(
                    "Unsupported WIF version byte {}",
                    other
                )))
            }
        }
        let key_bytes = &payload[1..];
        let compressed = key_bytes.len() == 33 && key_bytes[32] == 0x01;
        let raw = if compressed {
            &key_bytes[..32]
        } else {
            key_bytes
        };

        // The key material must be exactly 32 bytes (33 with the compression
        // flag). Reject anything else rather than panicking on copy_from_slice.
        if raw.len() != 32 {
            return Err(CoreError::InvalidWif(format!(
                "Invalid key length {}",
                raw.len()
            )));
        }

        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(raw);
        Self::from_bytes(bytes, compressed)
    }

    /// Encode the private key to WIF format using the given version byte.
    pub fn to_wif(&self, version_byte: u8) -> String {
        let mut payload = Vec::with_capacity(34);
        payload.push(version_byte);
        payload.extend_from_slice(&self.inner);
        if self.compressed {
            payload.push(0x01);
        }
        let check = checksum(&payload);
        payload.extend_from_slice(&check);
        bs58::encode(payload).into_string()
    }

    /// Get the raw 32-byte private key scalar.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.inner
    }

    /// Derive the corresponding public key.
    pub fn public_key(&self) -> Result<PublicKey> {
        let secp = Secp256k1::new();
        let secret = SecretKey::from_slice(&self.inner).map_err(CoreError::Secp256k1)?;
        Ok(PublicKey::from_secret_key(&secp, &secret))
    }

    pub fn is_compressed(&self) -> bool {
        self.compressed
    }
}

impl std::fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PrivateKey([REDACTED])")
    }
}

/// Serialize a public key to bytes (compressed or uncompressed).
pub fn serialize_pubkey(pubkey: &PublicKey, compressed: bool) -> Vec<u8> {
    if compressed {
        pubkey.serialize().to_vec()
    } else {
        pubkey.serialize_uncompressed().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::legacy;

    #[test]
    fn test_wif_roundtrip() {
        // Generate a random key and round-trip through WIF
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        // Ensure it's a valid secp256k1 key by trying until we get one
        let key = loop {
            if let Ok(k) = PrivateKey::from_bytes(bytes, true) {
                break k;
            }
            rand::thread_rng().fill_bytes(&mut bytes);
        };
        let wif = key.to_wif(legacy::SECRET_KEY_PREFIX);
        let recovered = PrivateKey::from_wif(&wif).expect("WIF decode failed");
        assert_eq!(key.as_bytes(), recovered.as_bytes());
    }
}
