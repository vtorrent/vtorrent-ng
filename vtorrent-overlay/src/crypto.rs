/// Cryptographic primitives for the overlay.
///
/// Key exchange:  X25519 Diffie-Hellman  (same as WireGuard)
/// Encryption:    ChaCha20-Poly1305 AEAD (same as WireGuard)
/// Node identity: Curve25519 public key  (32 bytes)
///
/// Wire format for an encrypted packet:
///   [4 bytes  nonce counter LE] [N bytes ciphertext+tag]
/// The full 96-bit nonce is: counter(4) || sender_pubkey[0..8]
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

use crate::error::{OverlayError, Result};

/// A node's long-term identity keypair.
#[derive(Clone)]
pub struct NodeKeypair {
    pub secret: StaticSecret,
    pub public: PublicKey,
}

impl NodeKeypair {
    /// Generate a new random keypair.
    pub fn generate() -> Self {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        Self { secret, public }
    }

    /// Load from raw 32-byte secret key bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        let secret = StaticSecret::from(bytes);
        let public = PublicKey::from(&secret);
        Self { secret, public }
    }

    /// Export the secret key bytes (for persistence).
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.secret.to_bytes()
    }

    /// The node ID is the hex-encoded public key.
    pub fn node_id(&self) -> String {
        hex::encode(self.public.as_bytes())
    }

    /// Raw X25519 DH against a remote public key (32 bytes, unhashed).
    pub fn dh_raw(&self, remote_public: &PublicKey) -> [u8; 32] {
        *self.secret.diffie_hellman(remote_public).as_bytes()
    }

    /// Derive a shared symmetric key with a remote public key.
    pub fn shared_key(&self, remote_public: &PublicKey) -> SharedKey {
        let dh = self.secret.diffie_hellman(remote_public);
        // Hash the DH output to get a uniform 32-byte key (HKDF-lite)
        let mut hasher = Sha256::new();
        hasher.update(dh.as_bytes());
        hasher.update(b"vtorrent-overlay-v1");
        let key_bytes: [u8; 32] = hasher.finalize().into();
        SharedKey(key_bytes)
    }
}

/// Domain-separated SHA256 over a sequence of byte slices.
///
/// Used for handshake MACs and session-key derivation. Length-prefixed
/// fields prevent concatenation ambiguity.
pub fn hash_transcript(domains: &[&[u8]], parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in domains.iter().chain(parts.iter()) {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// A symmetric session key derived from X25519 DH.
pub struct SharedKey([u8; 32]);

impl SharedKey {
    /// Create a SharedKey from raw bytes (used when the key was derived externally).
    pub fn from_raw(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl Drop for SharedKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl SharedKey {
    /// Encrypt a plaintext payload. Returns ciphertext with 4-byte nonce prefix.
    pub fn encrypt(
        &self,
        counter: u32,
        sender_pubkey: &[u8; 32],
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.0));
        let nonce = build_nonce(counter, sender_pubkey);
        let ct = cipher
            .encrypt(&nonce, plaintext)
            .map_err(|e| OverlayError::Crypto(e.to_string()))?;

        let mut out = Vec::with_capacity(4 + ct.len());
        out.extend_from_slice(&counter.to_le_bytes());
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Decrypt a payload produced by `encrypt`. Returns plaintext.
    pub fn decrypt(&self, sender_pubkey: &[u8; 32], packet: &[u8]) -> Result<Vec<u8>> {
        if packet.len() < 4 {
            return Err(OverlayError::Crypto("packet too short".into()));
        }
        let counter = u32::from_le_bytes([packet[0], packet[1], packet[2], packet[3]]);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.0));
        let nonce = build_nonce(counter, sender_pubkey);
        cipher
            .decrypt(&nonce, &packet[4..])
            .map_err(|e| OverlayError::Crypto(e.to_string()))
    }
}

/// Build a 12-byte ChaCha20 nonce from a 32-bit counter and the first 8 bytes
/// of the sender's public key. This is deterministic and unique per (session, counter).
fn build_nonce(counter: u32, sender_pubkey: &[u8; 32]) -> Nonce {
    let mut n = [0u8; 12];
    n[0..4].copy_from_slice(&counter.to_le_bytes());
    n[4..12].copy_from_slice(&sender_pubkey[0..8]);
    Nonce::from(n)
}

/// An ephemeral X25519 keypair used for one-shot key agreement during the
/// hole-punch handshake.
///
/// The handshake derives TWO shared secrets from each ephemeral (against the
/// peer's ephemeral and static keys, as in Noise KK), so the underlying
/// scalar is held as a reusable `StaticSecret`; single-use semantics are
/// enforced by consuming the keypair at derivation time.
pub struct EphemeralKeypair {
    secret: StaticSecret,
    public: [u8; 32],
}

impl EphemeralKeypair {
    /// Generate a fresh ephemeral keypair.
    pub fn generate() -> Self {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        Self {
            secret,
            public: *public.as_bytes(),
        }
    }

    /// The ephemeral public key to send to the peer.
    pub fn public_bytes(&self) -> [u8; 32] {
        self.public
    }

    /// Raw X25519 DH against a remote public key (32 bytes, unhashed).
    /// Consumes the keypair (single-use).
    pub fn dh_raw(self, remote_public: &[u8; 32]) -> [u8; 32] {
        let remote = PublicKey::from(*remote_public);
        *self.secret.diffie_hellman(&remote).as_bytes()
    }

    /// Derive the session key from our ephemeral secret and the peer's
    /// ephemeral public key. Consumes the keypair (single-use).
    pub fn shared_key(self, remote_public: &[u8; 32]) -> [u8; 32] {
        let shared = self.dh_raw(remote_public);
        let mut hasher = Sha256::new();
        hasher.update(shared);
        hasher.update(b"vtorrent-overlay-eph-v1");
        hasher.finalize().into()
    }
}

/// Derive the authenticated session key from the three DH outputs of the
/// Noise-KK-style handshake:
///   dh_ee = DH(eph_i, eph_r)      — forward secrecy
///   dh_es = DH(eph_i, static_r)   — responder authentication
///   dh_se = DH(static_i, eph_r)   — initiator authentication
///
/// An attacker who merely relays or races the handshake cannot compute any
/// of these without the corresponding static secrets.
pub fn derive_session_key(dh_ee: [u8; 32], dh_es: [u8; 32], dh_se: [u8; 32]) -> [u8; 32] {
    hash_transcript(&[b"vtorrent-overlay-sess-v2"], &[&dh_ee, &dh_es, &dh_se])
}

/// Compute all three handshake DH outputs as the INITIATOR.
///
/// Returns `(dh_ee, dh_es, dh_se)` where `es` mixes the RESPONDER's static
/// key and `se` mixes the INITIATOR's static key. Consumes the ephemeral.
pub fn initiator_handshake_dhs(
    eph: EphemeralKeypair,
    our_static: &NodeKeypair,
    peer_eph: &[u8; 32],
    peer_static: &[u8; 32],
) -> ([u8; 32], [u8; 32], [u8; 32]) {
    let peer_eph_pub = PublicKey::from(*peer_eph);
    let peer_static_pub = PublicKey::from(*peer_static);
    let ee = *eph.secret.diffie_hellman(&peer_eph_pub).as_bytes();
    let es = *eph.secret.diffie_hellman(&peer_static_pub).as_bytes();
    let se = our_static.dh_raw(&peer_eph_pub);
    (ee, es, se)
}

/// Compute all three handshake DH outputs as the RESPONDER.
///
/// Returns `(dh_ee, dh_es, dh_se)` matching [`derive_session_key`] argument
/// order (`es` = initiator-ephemeral × OUR static). Consumes the ephemeral.
pub fn responder_handshake_dhs(
    eph: EphemeralKeypair,
    our_static: &NodeKeypair,
    peer_eph: &[u8; 32],
    peer_static: &[u8; 32],
) -> ([u8; 32], [u8; 32], [u8; 32]) {
    let peer_eph_pub = PublicKey::from(*peer_eph);
    let peer_static_pub = PublicKey::from(*peer_static);
    let ee = *eph.secret.diffie_hellman(&peer_eph_pub).as_bytes();
    let es = our_static.dh_raw(&peer_eph_pub);
    let se = *eph.secret.diffie_hellman(&peer_static_pub).as_bytes();
    (ee, es, se)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keypair_generate_and_node_id() {
        let kp = NodeKeypair::generate();
        let id = kp.node_id();
        assert_eq!(id.len(), 64); // 32 bytes hex
    }

    #[test]
    fn test_shared_key_encrypt_decrypt_roundtrip() {
        let alice = NodeKeypair::generate();
        let bob = NodeKeypair::generate();

        let alice_shared = alice.shared_key(&bob.public);
        let bob_shared = bob.shared_key(&alice.public);

        let plaintext = b"hello vtorrent overlay";
        let ct = alice_shared
            .encrypt(1, alice.public.as_bytes(), plaintext)
            .unwrap();
        let pt = bob_shared.decrypt(alice.public.as_bytes(), &ct).unwrap();

        assert_eq!(pt, plaintext);
    }

    #[test]
    fn test_shared_key_wrong_counter_fails() {
        let alice = NodeKeypair::generate();
        let bob = NodeKeypair::generate();

        let alice_shared = alice.shared_key(&bob.public);
        let bob_shared = bob.shared_key(&alice.public);

        let plaintext = b"test";
        let mut ct = alice_shared
            .encrypt(1, alice.public.as_bytes(), plaintext)
            .unwrap();
        // Tamper with the counter
        ct[0] ^= 0xff;
        assert!(bob_shared.decrypt(alice.public.as_bytes(), &ct).is_err());
    }

    #[test]
    fn test_keypair_roundtrip_from_bytes() {
        let kp = NodeKeypair::generate();
        let bytes = kp.secret_bytes();
        let kp2 = NodeKeypair::from_bytes(bytes);
        assert_eq!(kp.public.as_bytes(), kp2.public.as_bytes());
    }

    #[test]
    fn test_ephemeral_dh_produces_32_bytes() {
        let a = EphemeralKeypair::generate();
        let b = EphemeralKeypair::generate();
        let a_pub = a.public_bytes();
        let b_pub = b.public_bytes();
        let ka = a.shared_key(&b_pub);
        let kb = b.shared_key(&a_pub);
        assert_eq!(ka.len(), 32);
        assert_eq!(kb.len(), 32);
        assert_eq!(ka, kb);
    }
}
