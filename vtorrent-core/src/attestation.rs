//! Signed attestation that a funding outpoint is confirmed and unspent.
//!
//! The offline cold-stake signer has no node, so a node it trusts signs the
//! facts it needs. The signature covers a fixed byte encoding of every field
//! that affects the decision, so a field cannot be changed after signing.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const ATTESTATION_DOMAIN: &[u8] = b"vtorrent-cold-stake-funding-attestation-v1";
pub const ATTESTATION_VERSION: u8 = 1;
/// Clock skew tolerated between the issuing node and the offline signer.
pub const MAX_CLOCK_SKEW_SECONDS: u64 = 300;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FundingAttestation {
    pub version: u8,
    pub network: String,
    pub txid: String,
    pub vout: u32,
    pub value_satoshis: u64,
    pub script_pubkey: String,
    pub block_hash: String,
    pub block_height: u32,
    pub confirmations: u32,
    pub tip_hash: String,
    pub tip_height: u32,
    pub mempool_spend: bool,
    pub issued_at: u64,
    pub expires_at: u64,
    pub attestor_pubkey: String,
    pub signature: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AttestationError {
    #[error("attestation field is malformed: {0}")]
    Malformed(&'static str),
    #[error("attestation signature does not verify")]
    BadSignature,
    #[error("attestation was signed by a different key than the pinned one")]
    WrongKey,
    #[error("attestation network {found} does not match {expected}")]
    Network { found: String, expected: String },
    #[error("attestation expired at {expires_at}; local clock reads {now}")]
    Expired { expires_at: u64, now: u64 },
    #[error("attestation was issued at {issued_at}, ahead of the local clock {now}")]
    IssuedInFuture { issued_at: u64, now: u64 },
    #[error("attestation field {0} does not match the funding transaction")]
    Field(&'static str),
    #[error("attestation records a mempool spend of the funding outpoint")]
    MempoolSpend,
    #[error("attestation confirms only {confirmations} blocks; {required} required")]
    TooFewConfirmations { confirmations: u32, required: u32 },
}

/// Largest single field accepted in the signed encoding.
const MAX_FIELD_BYTES: usize = 10_000;

fn push_bytes(
    buffer: &mut Vec<u8>,
    bytes: &[u8],
    field: &'static str,
) -> Result<(), AttestationError> {
    if bytes.len() > MAX_FIELD_BYTES {
        return Err(AttestationError::Malformed(field));
    }
    let length = u32::try_from(bytes.len()).map_err(|_| AttestationError::Malformed(field))?;
    buffer.extend_from_slice(&length.to_le_bytes());
    buffer.extend_from_slice(bytes);
    Ok(())
}

fn hex_field(
    value: &str,
    field: &'static str,
    expected_length: Option<usize>,
) -> Result<Vec<u8>, AttestationError> {
    let bytes = hex::decode(value).map_err(|_| AttestationError::Malformed(field))?;
    if expected_length.is_some_and(|length| bytes.len() != length) {
        return Err(AttestationError::Malformed(field));
    }
    Ok(bytes)
}

impl FundingAttestation {
    /// Canonical bytes covered by the signature.
    ///
    /// Field order is fixed and every variable-length value carries its length,
    /// so moving bytes between fields cannot produce the same encoding.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, AttestationError> {
        let mut buffer = Vec::new();
        push_bytes(&mut buffer, ATTESTATION_DOMAIN, "domain")?;
        push_bytes(&mut buffer, self.network.as_bytes(), "network")?;
        buffer.push(self.version);
        for (value, field, length) in [
            (&self.txid, "txid", Some(32)),
            (&self.script_pubkey, "script_pubkey", None),
            (&self.block_hash, "block_hash", Some(32)),
            (&self.tip_hash, "tip_hash", Some(32)),
        ] {
            push_bytes(&mut buffer, &hex_field(value, field, length)?, field)?;
        }
        buffer.extend_from_slice(&self.vout.to_le_bytes());
        buffer.extend_from_slice(&self.value_satoshis.to_le_bytes());
        buffer.extend_from_slice(&self.block_height.to_le_bytes());
        buffer.extend_from_slice(&self.confirmations.to_le_bytes());
        buffer.extend_from_slice(&self.tip_height.to_le_bytes());
        buffer.push(u8::from(self.mempool_spend));
        buffer.extend_from_slice(&self.issued_at.to_le_bytes());
        buffer.extend_from_slice(&self.expires_at.to_le_bytes());
        Ok(buffer)
    }

    fn digest(&self) -> Result<[u8; 32], AttestationError> {
        Ok(Sha256::digest(self.signing_bytes()?).into())
    }

    pub fn sign(&mut self, key: &secp256k1::SecretKey) -> Result<(), AttestationError> {
        let secp = secp256k1::Secp256k1::new();
        let public = secp256k1::PublicKey::from_secret_key(&secp, key);
        let message = secp256k1::Message::from_digest(self.digest()?);
        let signature = secp.sign_ecdsa(&message, key);
        self.attestor_pubkey = hex::encode(public.serialize());
        self.signature = hex::encode(signature.serialize_der());
        Ok(())
    }

    pub fn verify_signature(&self, pinned_pubkey_hex: &str) -> Result<(), AttestationError> {
        if !self.attestor_pubkey.eq_ignore_ascii_case(pinned_pubkey_hex) {
            return Err(AttestationError::WrongKey);
        }
        let public_bytes = hex::decode(pinned_pubkey_hex)
            .map_err(|_| AttestationError::Malformed("attestor_pubkey"))?;
        let public = secp256k1::PublicKey::from_slice(&public_bytes)
            .map_err(|_| AttestationError::Malformed("attestor_pubkey"))?;
        let signature_bytes =
            hex::decode(&self.signature).map_err(|_| AttestationError::Malformed("signature"))?;
        let signature = secp256k1::ecdsa::Signature::from_der(&signature_bytes)
            .map_err(|_| AttestationError::BadSignature)?;
        secp256k1::Secp256k1::verification_only()
            .verify_ecdsa(
                &secp256k1::Message::from_digest(self.digest()?),
                &signature,
                &public,
            )
            .map_err(|_| AttestationError::BadSignature)
    }

    pub fn parse(json: &[u8]) -> Result<Self, AttestationError> {
        serde_json::from_slice(json).map_err(|_| AttestationError::Malformed("json"))
    }

    pub fn check_funding(
        &self,
        txid_hex: &str,
        vout: u32,
        value_satoshis: u64,
        script_pubkey_hex: &str,
    ) -> Result<(), AttestationError> {
        if !self.txid.eq_ignore_ascii_case(txid_hex) {
            return Err(AttestationError::Field("txid"));
        }
        if self.vout != vout {
            return Err(AttestationError::Field("vout"));
        }
        if self.value_satoshis != value_satoshis {
            return Err(AttestationError::Field("value_satoshis"));
        }
        if !self.script_pubkey.eq_ignore_ascii_case(script_pubkey_hex) {
            return Err(AttestationError::Field("script_pubkey"));
        }
        Ok(())
    }

    pub fn check_policy(
        &self,
        expected_network: &str,
        min_confirmations: u32,
        now: u64,
    ) -> Result<(), AttestationError> {
        if self.version != ATTESTATION_VERSION {
            return Err(AttestationError::Malformed("version"));
        }
        if self.network != expected_network {
            return Err(AttestationError::Network {
                found: self.network.clone(),
                expected: expected_network.to_owned(),
            });
        }
        if self.mempool_spend {
            return Err(AttestationError::MempoolSpend);
        }
        if self.confirmations < min_confirmations {
            return Err(AttestationError::TooFewConfirmations {
                confirmations: self.confirmations,
                required: min_confirmations,
            });
        }
        if self.issued_at > now.saturating_add(MAX_CLOCK_SKEW_SECONDS) {
            return Err(AttestationError::IssuedInFuture {
                issued_at: self.issued_at,
                now,
            });
        }
        if now > self.expires_at {
            return Err(AttestationError::Expired {
                expires_at: self.expires_at,
                now,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FundingAttestation {
        FundingAttestation {
            version: ATTESTATION_VERSION,
            network: "vtorrent-regtest".into(),
            txid: "11".repeat(32),
            vout: 0,
            value_satoshis: 100_000_000,
            script_pubkey: "22".repeat(8),
            block_hash: "33".repeat(32),
            block_height: 100,
            confirmations: 6,
            tip_hash: "44".repeat(32),
            tip_height: 105,
            mempool_spend: false,
            issued_at: 1_000,
            expires_at: 4_600,
            attestor_pubkey: String::new(),
            signature: String::new(),
        }
    }

    fn key(byte: u8) -> secp256k1::SecretKey {
        secp256k1::SecretKey::from_slice(&[byte; 32]).unwrap()
    }

    fn pubkey_hex(key: &secp256k1::SecretKey) -> String {
        let secp = secp256k1::Secp256k1::new();
        hex::encode(secp256k1::PublicKey::from_secret_key(&secp, key).serialize())
    }

    fn mutations() -> Vec<fn(&mut FundingAttestation)> {
        vec![
            |a| a.network = "vtorrent-mainnet".into(),
            |a| a.txid = "55".repeat(32),
            |a| a.vout = 1,
            |a| a.value_satoshis += 1,
            |a| a.script_pubkey = "66".repeat(8),
            |a| a.block_hash = "77".repeat(32),
            |a| a.block_height += 1,
            |a| a.confirmations += 1,
            |a| a.tip_hash = "88".repeat(32),
            |a| a.tip_height += 1,
            |a| a.mempool_spend = true,
            |a| a.issued_at += 1,
            |a| a.expires_at += 1,
            |a| a.version = a.version.wrapping_add(1),
        ]
    }

    #[test]
    fn round_trip_signs_and_verifies() {
        let signer = key(7);
        let mut attestation = sample();
        attestation.sign(&signer).unwrap();
        attestation.verify_signature(&pubkey_hex(&signer)).unwrap();
    }

    #[test]
    fn tampering_with_any_signed_field_fails_verification() {
        let signer = key(7);
        let pinned = pubkey_hex(&signer);
        for mutate in mutations() {
            let mut attestation = sample();
            attestation.sign(&signer).unwrap();
            mutate(&mut attestation);
            assert!(
                attestation.verify_signature(&pinned).is_err(),
                "a changed field must invalidate the signature"
            );
        }
    }

    #[test]
    fn malformed_hex_or_wrong_length_hashes_cannot_be_signed() {
        let signer = key(7);
        for mutate in [
            (|a: &mut FundingAttestation| a.txid = "zz".into()) as fn(&mut FundingAttestation),
            |a| a.txid = "11".into(),
            |a| a.block_hash = "not-hex".into(),
            |a| a.tip_hash = "22".into(),
            |a| a.script_pubkey = "xy".into(),
        ] {
            let mut attestation = sample();
            mutate(&mut attestation);
            assert!(
                attestation.sign(&signer).is_err(),
                "a malformed field must not be signed"
            );
        }
    }

    #[test]
    fn policy_rejects_an_attestation_issued_far_ahead_of_the_local_clock() {
        let attestation = sample();
        let early = attestation.issued_at - MAX_CLOCK_SKEW_SECONDS - 1;
        assert_eq!(
            attestation.check_policy(&attestation.network, 6, early),
            Err(AttestationError::IssuedInFuture {
                issued_at: attestation.issued_at,
                now: early
            })
        );
        assert!(attestation
            .check_policy(
                &attestation.network,
                6,
                attestation.issued_at - MAX_CLOCK_SKEW_SECONDS
            )
            .is_ok());
    }

    #[test]
    fn policy_rejects_an_unknown_version() {
        let mut attestation = sample();
        attestation.version = ATTESTATION_VERSION + 1;
        assert_eq!(
            attestation.check_policy(&attestation.network, 6, attestation.issued_at),
            Err(AttestationError::Malformed("version"))
        );
    }

    #[test]
    fn a_different_pinned_key_is_rejected() {
        let signer = key(7);
        let mut attestation = sample();
        attestation.sign(&signer).unwrap();
        assert_eq!(
            attestation.verify_signature(&pubkey_hex(&key(9))),
            Err(AttestationError::WrongKey)
        );
    }

    #[test]
    fn unknown_or_missing_json_fields_are_rejected() {
        let mut value = serde_json::to_value(sample()).unwrap();
        value["extra"] = serde_json::json!(true);
        assert!(FundingAttestation::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        value.as_object_mut().unwrap().remove("tip_hash");
        assert!(FundingAttestation::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn a_signature_with_trailing_bytes_is_rejected() {
        let signer = key(7);
        let mut attestation = sample();
        attestation.sign(&signer).unwrap();
        attestation.signature.push_str("00");
        assert_eq!(
            attestation.verify_signature(&pubkey_hex(&signer)),
            Err(AttestationError::BadSignature)
        );
    }

    #[test]
    fn funding_mismatches_name_the_field() {
        let attestation = sample();
        assert_eq!(
            attestation.check_funding(&"99".repeat(32), 0, 100_000_000, &"22".repeat(8)),
            Err(AttestationError::Field("txid"))
        );
        assert_eq!(
            attestation.check_funding(&attestation.txid, 1, 100_000_000, &"22".repeat(8)),
            Err(AttestationError::Field("vout"))
        );
        assert_eq!(
            attestation.check_funding(&attestation.txid, 0, 100_000_001, &"22".repeat(8)),
            Err(AttestationError::Field("value_satoshis"))
        );
        assert_eq!(
            attestation.check_funding(&attestation.txid, 0, 100_000_000, &"33".repeat(8)),
            Err(AttestationError::Field("script_pubkey"))
        );
    }

    #[test]
    fn policy_rejects_mempool_spend_low_confirmations_and_expiry() {
        let attestation = sample();
        assert_eq!(
            attestation.check_policy(&attestation.network, 6, attestation.issued_at),
            Ok(())
        );
        let mut spent = sample();
        spent.mempool_spend = true;
        assert_eq!(
            spent.check_policy(&spent.network, 6, spent.issued_at),
            Err(AttestationError::MempoolSpend)
        );
        let mut shallow = sample();
        shallow.confirmations = 5;
        assert_eq!(
            shallow.check_policy(&shallow.network, 6, shallow.issued_at),
            Err(AttestationError::TooFewConfirmations {
                confirmations: 5,
                required: 6
            })
        );
        let stale = sample();
        let after = stale.expires_at + 1;
        assert_eq!(
            stale.check_policy(&stale.network, 6, after),
            Err(AttestationError::Expired {
                expires_at: stale.expires_at,
                now: after
            })
        );
    }

    #[test]
    fn policy_rejects_a_different_network() {
        let attestation = sample();
        assert_eq!(
            attestation.check_policy("vtorrent-mainnet", 6, attestation.issued_at),
            Err(AttestationError::Network {
                found: attestation.network.clone(),
                expected: "vtorrent-mainnet".into()
            })
        );
    }
}
