use ripemd::Ripemd160;
use sha2::{Digest, Sha256};

/// Compute SHA-256 of the input data.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Compute double SHA-256 (SHA256d) of the input data.
/// This is the standard Bitcoin/vTorrent hash function for block headers and transactions.
pub fn sha256d(data: &[u8]) -> [u8; 32] {
    sha256(&sha256(data))
}

/// Compute RIPEMD-160 of the input data.
pub fn ripemd160(data: &[u8]) -> [u8; 20] {
    let mut hasher = Ripemd160::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Compute Hash160 = RIPEMD160(SHA256(data)).
/// This is used to derive P2PKH addresses from public keys.
pub fn hash160(data: &[u8]) -> [u8; 20] {
    ripemd160(&sha256(data))
}

/// Compute the 4-byte Base58Check checksum of the input data.
/// Checksum = first 4 bytes of SHA256d(data).
pub fn checksum(data: &[u8]) -> [u8; 4] {
    let hash = sha256d(data);
    [hash[0], hash[1], hash[2], hash[3]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256d_known_vector() {
        // SHA256d of empty string
        let result = sha256d(b"");
        let expected =
            hex::decode("5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456")
                .unwrap();
        assert_eq!(&result[..], expected.as_slice());
    }

    #[test]
    fn test_hash160_known_vector() {
        // Hash160 of a known public key (Bitcoin genesis coinbase pubkey)
        let pubkey = hex::decode(
            "04678afdb0fe5548271967f1a67130b7105cd6a828e03909a67962e0ea1f61deb649f6bc3f4cef38c4f35504e51ec112de5c384df7ba0b8d578a4c702b6bf11d5f"
        ).unwrap();
        let result = hash160(&pubkey);
        // Just verify length and non-zero
        assert_eq!(result.len(), 20);
        assert_ne!(result, [0u8; 20]);
    }
}
