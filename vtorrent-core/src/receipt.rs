//! Signed bandwidth receipts for torrent incentive verification.
//!
//! The torrent incentive pays peers for bandwidth, but the accounting is
//! unilateral: each side counts bytes it observed and pays on its own numbers.
//! A receipt is a **signed, bilateral** statement of the bytes observed in one
//! settlement window, so a payment can require the two sides to agree.
//!
//! See `docs/incentive-verification-design.md`. This module is the pure,
//! deterministic core: build the canonical message, sign it with a VTR key, and
//! verify it against the signer's address.

use crate::address::Address;
use crate::crypto::sha256;
use crate::error::{CoreError, Result};
use crate::keys::PrivateKey;
use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};

/// Domain-separation prefix so a receipt signature can never be replayed as a
/// transaction signature (or any other signed message).
pub const RECEIPT_DOMAIN: &[u8] = b"vTorrent/BandwidthReceipt/v1";

/// A signed statement of bytes observed in one settlement window.
///
/// `uploaded_by_me` / `downloaded_by_me` are from the **signer's** point of view:
/// the signer asserts how many bytes it sent to, and received from, `peer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BandwidthReceipt {
    /// Torrent info-hash this window belongs to.
    pub info_hash: [u8; 20],
    /// Window start (unix seconds).
    pub window_start: u64,
    /// Window end (unix seconds).
    pub window_end: u64,
    /// Bytes the signer uploaded to `peer`.
    pub uploaded_by_me: u64,
    /// Bytes the signer downloaded from `peer`.
    pub downloaded_by_me: u64,
    /// The counterparty's VTR address (who this statement is about).
    pub peer: String,
    /// The signer's VTR address (whose key signed this).
    pub signer: String,
    /// Monotonic settlement id — makes a receipt single-use (anti-replay).
    pub settlement_id: u64,
}

impl BandwidthReceipt {
    /// The canonical bytes that are signed/verified.
    ///
    /// Deterministic and domain-separated; every field is length-prefixed so no
    /// two distinct receipts can collide.
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(160);
        buf.extend_from_slice(RECEIPT_DOMAIN);
        buf.extend_from_slice(&self.info_hash);
        buf.extend_from_slice(&self.window_start.to_le_bytes());
        buf.extend_from_slice(&self.window_end.to_le_bytes());
        buf.extend_from_slice(&self.uploaded_by_me.to_le_bytes());
        buf.extend_from_slice(&self.downloaded_by_me.to_le_bytes());
        for s in [&self.peer, &self.signer] {
            let b = s.as_bytes();
            buf.extend_from_slice(&(b.len() as u32).to_le_bytes());
            buf.extend_from_slice(b);
        }
        buf.extend_from_slice(&self.settlement_id.to_le_bytes());
        buf
    }

    /// The digest that is signed.
    pub fn digest(&self) -> [u8; 32] {
        sha256(&self.signing_payload())
    }

    /// Sign this receipt with `key`, returning the DER-encoded signature.
    pub fn sign(&self, key: &PrivateKey) -> Result<Vec<u8>> {
        let secp = Secp256k1::new();
        let secret =
            SecretKey::from_slice(key.as_bytes()).map_err(|_| CoreError::InvalidPrivateKey)?;
        let message = Message::from_digest(self.digest());
        let sig = secp.sign_ecdsa(&message, &secret);
        Ok(sig.serialize_der().to_vec())
    }

    /// Verify `signature` against an explicit compressed `pubkey`, checking the
    /// pubkey hashes to `self.signer`.
    ///
    /// Returns `Ok(true)` on a valid signature, `Ok(false)` on a well-formed but
    /// non-matching signature, and `Err` on malformed input.
    pub fn verify_with_pubkey(&self, pubkey: &[u8], signature: &[u8]) -> Result<bool> {
        let expected = Address::parse(&self.signer)?;
        let pk = PublicKey::from_slice(pubkey).map_err(|_| CoreError::InvalidPublicKey)?;
        let derived = Address::from_pubkey(&pk, true, expected.version);
        if derived.to_string() != self.signer {
            return Ok(false);
        }
        let secp = Secp256k1::new();
        let sig = secp256k1::ecdsa::Signature::from_der(signature)
            .map_err(|_| CoreError::InvalidAddress("malformed signature".into()))?;
        let message = Message::from_digest(self.digest());
        Ok(secp.verify_ecdsa(&message, &sig, &pk).is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::mainnet::PUBKEY_ADDRESS_PREFIX;

    fn key(byte: u8) -> PrivateKey {
        PrivateKey::from_bytes([byte; 32], true).unwrap()
    }

    fn addr(key: &PrivateKey) -> String {
        let pk = key.public_key().unwrap();
        Address::from_pubkey(&pk, true, PUBKEY_ADDRESS_PREFIX).to_string()
    }

    fn receipt(signer: &PrivateKey, peer: &PrivateKey) -> BandwidthReceipt {
        BandwidthReceipt {
            info_hash: [7u8; 20],
            window_start: 1_000,
            window_end: 1_300,
            uploaded_by_me: 10 * 1024 * 1024,
            downloaded_by_me: 5 * 1024 * 1024,
            peer: addr(peer),
            signer: addr(signer),
            settlement_id: 42,
        }
    }

    #[test]
    fn sign_verify_roundtrip() {
        let a = key(1);
        let b = key(2);
        let r = receipt(&a, &b);
        let sig = r.sign(&a).unwrap();
        let pk = a.public_key().unwrap().serialize();
        assert!(r.verify_with_pubkey(&pk, &sig).unwrap());
    }

    #[test]
    fn wrong_key_rejected() {
        let a = key(1);
        let b = key(2);
        let r = receipt(&a, &b);
        let sig = r.sign(&a).unwrap();
        // b's pubkey does not hash to r.signer (a), so verification is false.
        let pk_b = b.public_key().unwrap().serialize();
        assert!(!r.verify_with_pubkey(&pk_b, &sig).unwrap());
    }

    #[test]
    fn tampered_amount_rejected() {
        let a = key(1);
        let b = key(2);
        let mut r = receipt(&a, &b);
        let sig = r.sign(&a).unwrap();
        r.uploaded_by_me += 1; // inflate the claim
        let pk = a.public_key().unwrap().serialize();
        assert!(!r.verify_with_pubkey(&pk, &sig).unwrap());
    }

    #[test]
    fn domain_separated_from_other_messages() {
        let a = key(1);
        let b = key(2);
        let r = receipt(&a, &b);
        assert!(r.signing_payload().starts_with(RECEIPT_DOMAIN));
    }
}
