//! Message signing and proof of address ownership.
//!
//! Sign an arbitrary message with an address's key; anyone can verify by
//! recovering the public key from the signature and checking it hashes to the
//! address. Domain-separated so a signed message can never be replayed as a
//! transaction signature. See `docs/message-signing-design.md`.

use crate::address::Address;
use crate::crypto::sha256d;
use crate::error::{CoreError, Result};
use crate::keys::PrivateKey;
use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};

/// Domain-separation prefix (Bitcoin-style, with a length byte).
pub const MESSAGE_PREFIX: &[u8] = b"\x18vTorrent Signed Message:\n";

/// A signed message: the signature plus the recovery id, base64-encoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedMessage {
    /// Base64 of `[recovery_id || r || s]` (65 bytes).
    pub signature: String,
    /// The address that signed (for convenience; verification re-derives it).
    pub address: String,
    /// The message that was signed.
    pub message: String,
}

/// The digest that is signed: `sha256d(prefix || varint(len) || message)`.
pub fn message_digest(message: &str) -> [u8; 32] {
    let msg = message.as_bytes();
    let mut buf = Vec::with_capacity(MESSAGE_PREFIX.len() + 9 + msg.len());
    buf.extend_from_slice(MESSAGE_PREFIX);
    // Bitcoin-style varint length.
    let len = msg.len() as u64;
    if len < 0xfd {
        buf.push(len as u8);
    } else if len <= 0xffff {
        buf.push(0xfd);
        buf.extend_from_slice(&(len as u16).to_le_bytes());
    } else {
        buf.push(0xfe);
        buf.extend_from_slice(&(len as u32).to_le_bytes());
    }
    buf.extend_from_slice(msg);
    sha256d(&buf)
}

/// Sign `message` with `key`, producing a base64 recoverable signature.
pub fn sign_message(key: &PrivateKey, message: &str) -> Result<SignedMessage> {
    let secp = Secp256k1::new();
    let secret = SecretKey::from_slice(key.as_bytes()).map_err(|_| CoreError::InvalidPrivateKey)?;
    let digest = Message::from_digest(message_digest(message));
    let rec = secp.sign_ecdsa_recoverable(&digest, &secret);
    let (rec_id, sig) = rec.serialize_compact();
    let mut bytes = Vec::with_capacity(65);
    bytes.push(rec_id.to_i32() as u8);
    bytes.extend_from_slice(&sig);
    let pk = key.public_key()?;
    let address = Address::from_pubkey(&pk, true, crate::network::mainnet::PUBKEY_ADDRESS_PREFIX);
    Ok(SignedMessage {
        signature: base64_encode(&bytes),
        address: address.to_string(),
        message: message.to_string(),
    })
}

/// Verify a signed message: recover the pubkey and check it hashes to `address`.
///
/// Returns `Ok(true)` if the signature is valid for `address`, `Ok(false)` if it
/// is well-formed but does not match, and `Err` on malformed input.
pub fn verify_message(address: &str, message: &str, signature_b64: &str) -> Result<bool> {
    let expected = Address::parse(address)?;
    let bytes = base64_decode(signature_b64)
        .ok_or_else(|| CoreError::InvalidAddress("signature is not valid base64".into()))?;
    if bytes.len() != 65 {
        return Err(CoreError::InvalidAddress(format!(
            "signature must be 65 bytes, got {}",
            bytes.len()
        )));
    }
    let rec_id = secp256k1::ecdsa::RecoveryId::from_i32(bytes[0] as i32)
        .map_err(|_| CoreError::InvalidAddress("invalid recovery id".into()))?;
    let mut sig = [0u8; 64];
    sig.copy_from_slice(&bytes[1..65]);
    let rec = secp256k1::ecdsa::RecoverableSignature::from_compact(&sig, rec_id)
        .map_err(|_| CoreError::InvalidAddress("malformed signature".into()))?;
    let secp = Secp256k1::new();
    let digest = Message::from_digest(message_digest(message));
    let pk: PublicKey = secp
        .recover_ecdsa(&digest, &rec)
        .map_err(|_| CoreError::InvalidAddress("could not recover public key".into()))?;
    let derived = Address::from_pubkey(&pk, true, expected.version);
    Ok(derived.to_string() == address)
}

// ─── Minimal base64 (no external dep) ─────────────────────────────────────────

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[((n >> 18) & 63) as usize] as char);
        out.push(B64[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut rev = [255u8; 256];
    for (i, &c) in B64.iter().enumerate() {
        rev[c as usize] = i as u8;
    }
    let bytes: Vec<u8> = s.bytes().filter(|&b| b != b'\n' && b != b'\r').collect();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut vals = [0u8; 4];
        let mut pad = 0;
        for (i, &c) in chunk.iter().enumerate() {
            if c == b'=' {
                pad += 1;
                vals[i] = 0;
            } else {
                let v = rev[c as usize];
                if v == 255 {
                    return None;
                }
                vals[i] = v;
            }
        }
        let n = ((vals[0] as u32) << 18)
            | ((vals[1] as u32) << 12)
            | ((vals[2] as u32) << 6)
            | vals[3] as u32;
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> PrivateKey {
        PrivateKey::from_bytes([byte; 32], true).unwrap()
    }

    #[test]
    fn sign_verify_roundtrip() {
        let k = key(1);
        let signed = sign_message(&k, "I own this address").unwrap();
        assert!(verify_message(&signed.address, "I own this address", &signed.signature).unwrap());
    }

    #[test]
    fn wrong_message_fails() {
        let k = key(1);
        let signed = sign_message(&k, "hello").unwrap();
        assert!(!verify_message(&signed.address, "goodbye", &signed.signature).unwrap());
    }

    #[test]
    fn wrong_address_fails() {
        let k = key(1);
        let other = key(2);
        let other_addr = Address::from_pubkey(
            &other.public_key().unwrap(),
            true,
            crate::network::mainnet::PUBKEY_ADDRESS_PREFIX,
        )
        .to_string();
        let signed = sign_message(&k, "hello").unwrap();
        assert!(!verify_message(&other_addr, "hello", &signed.signature).unwrap());
    }

    #[test]
    fn malformed_signature_rejected() {
        let k = key(1);
        let addr = sign_message(&k, "x").unwrap().address;
        assert!(verify_message(&addr, "x", "not base64!!!").is_err());
        assert!(verify_message(&addr, "x", "AAAA").is_err()); // wrong length
    }

    #[test]
    fn domain_separated_digest() {
        // The digest must include the vTorrent prefix, so it cannot equal a
        // bare sha256d of the message (which a tx sighash could resemble).
        let d = message_digest("hello");
        let bare = sha256d(b"hello");
        assert_ne!(d, bare);
    }

    #[test]
    fn base64_roundtrip() {
        for data in [vec![], vec![1], vec![1, 2], vec![1, 2, 3], vec![255; 65]] {
            let enc = base64_encode(&data);
            assert_eq!(base64_decode(&enc).unwrap(), data);
        }
    }
}
