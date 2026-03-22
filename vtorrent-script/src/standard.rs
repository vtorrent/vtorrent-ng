//! Standard script type builders and classifier.
//!
//! Provides:
//! - `classify_script()` — identify the type of a scriptPubKey
//! - `build_p2pkh()` — Pay-to-Public-Key-Hash output script
//! - `build_p2sh()` — Pay-to-Script-Hash output script
//! - `build_p2ms()` — M-of-N multisig output script
//! - `build_htlc()` — Hash Time-Locked Contract (atomic swap) script
//! - `build_op_return()` — OP_RETURN data carrier output

use crate::script::Script;
use crate::error::{Result, ScriptError};

/// The type of a standard scriptPubKey.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptType {
    /// Pay-to-Public-Key-Hash: OP_DUP OP_HASH160 <20-byte hash> OP_EQUALVERIFY OP_CHECKSIG
    P2PKH,
    /// Pay-to-Public-Key: <pubkey> OP_CHECKSIG
    P2PK,
    /// Pay-to-Script-Hash: OP_HASH160 <20-byte hash> OP_EQUAL
    P2SH,
    /// M-of-N Multisig: OP_M <key1> ... <keyN> OP_N OP_CHECKMULTISIG
    P2MS { m: u8, n: u8 },
    /// Hash Time-Locked Contract (HTLC) for atomic swaps
    Htlc,
    /// OP_RETURN data carrier (unspendable)
    OpReturn,
    /// Unknown or non-standard script
    NonStandard,
}

/// Classify a scriptPubKey into its standard type.
pub fn classify_script(script: &Script) -> ScriptType {
    let b = script.as_bytes();

    // P2PKH: OP_DUP OP_HASH160 <20> OP_EQUALVERIFY OP_CHECKSIG
    if b.len() == 25 && b[0] == 0x76 && b[1] == 0xa9 && b[2] == 0x14 && b[23] == 0x88 && b[24] == 0xac {
        return ScriptType::P2PKH;
    }

    // P2SH: OP_HASH160 <20> OP_EQUAL
    if b.len() == 23 && b[0] == 0xa9 && b[1] == 0x14 && b[22] == 0x87 {
        return ScriptType::P2SH;
    }

    // P2PK: <33 or 65 bytes> OP_CHECKSIG
    if (b.len() == 35 && b[0] == 33 && b[34] == 0xac)
        || (b.len() == 67 && b[0] == 65 && b[66] == 0xac)
    {
        return ScriptType::P2PK;
    }

    // OP_RETURN
    if !b.is_empty() && b[0] == 0x6a {
        return ScriptType::OpReturn;
    }

    // P2MS: OP_M <key1> ... <keyN> OP_N OP_CHECKMULTISIG
    if b.len() >= 3 && b[0] >= 0x51 && b[0] <= 0x60 {
        let m = b[0] - 0x50;
        if let Some(&last) = b.last() {
            if last == 0xae {
                let n_byte = b[b.len() - 2];
                if n_byte >= 0x51 && n_byte <= 0x60 {
                    let n = n_byte - 0x50;
                    if m <= n {
                        return ScriptType::P2MS { m, n };
                    }
                }
            }
        }
    }

    // HTLC: starts with OP_IF OP_SHA256 (simplified detection)
    if b.len() > 5 && b[0] == 0x63 && b[1] == 0xa8 {
        return ScriptType::Htlc;
    }

    ScriptType::NonStandard
}

/// Build a P2PKH scriptPubKey from a 20-byte public key hash.
///
/// Output: `OP_DUP OP_HASH160 <hash> OP_EQUALVERIFY OP_CHECKSIG`
pub fn build_p2pkh(pubkey_hash: &[u8; 20]) -> Script {
    let mut s = Script::new();
    s.push_opcode(0x76); // OP_DUP
    s.push_opcode(0xa9); // OP_HASH160
    s.push_data(pubkey_hash).unwrap();
    s.push_opcode(0x88); // OP_EQUALVERIFY
    s.push_opcode(0xac); // OP_CHECKSIG
    s
}

/// Build a P2SH scriptPubKey from a 20-byte redeem script hash.
///
/// Output: `OP_HASH160 <hash> OP_EQUAL`
pub fn build_p2sh(script_hash: &[u8; 20]) -> Script {
    let mut s = Script::new();
    s.push_opcode(0xa9); // OP_HASH160
    s.push_data(script_hash).unwrap();
    s.push_opcode(0x87); // OP_EQUAL
    s
}

/// Build an M-of-N multisig scriptPubKey.
///
/// Output: `OP_M <key1> ... <keyN> OP_N OP_CHECKMULTISIG`
///
/// # Errors
/// Returns an error if `m > n` or `n > 15`.
pub fn build_p2ms(m: u8, keys: &[Vec<u8>]) -> Result<Script> {
    let n = keys.len() as u8;
    if m > n {
        return Err(ScriptError::MultisigKeyCount(m as usize, n as usize));
    }
    if n > 15 {
        return Err(ScriptError::MultisigKeyCount(m as usize, n as usize));
    }

    let mut s = Script::new();
    s.push_int(m);
    for key in keys {
        s.push_data(key).unwrap();
    }
    s.push_int(n);
    s.push_opcode(0xae); // OP_CHECKMULTISIG
    Ok(s)
}

/// Build an HTLC (Hash Time-Locked Contract) scriptPubKey for atomic swaps.
///
/// The script allows:
/// - **Claim path**: provide the preimage of `payment_hash` (before `expiry`)
/// - **Refund path**: provide a signature from `refund_pubkey` (after `expiry`)
///
/// Script structure:
/// ```text
/// OP_IF
///   OP_SHA256 <payment_hash> OP_EQUALVERIFY
///   OP_DUP OP_HASH160 <claim_pubkey_hash> OP_EQUALVERIFY OP_CHECKSIG
/// OP_ELSE
///   <expiry> OP_CHECKLOCKTIMEVERIFY OP_DROP
///   OP_DUP OP_HASH160 <refund_pubkey_hash> OP_EQUALVERIFY OP_CHECKSIG
/// OP_ENDIF
/// ```
pub fn build_htlc(
    payment_hash: &[u8; 32],
    claim_pubkey_hash: &[u8; 20],
    refund_pubkey_hash: &[u8; 20],
    expiry: u32,
) -> Script {
    let mut s = Script::new();

    // Claim branch
    s.push_opcode(0x63); // OP_IF
    s.push_opcode(0xa8); // OP_SHA256
    s.push_data(payment_hash).unwrap();
    s.push_opcode(0x88); // OP_EQUALVERIFY
    s.push_opcode(0x76); // OP_DUP
    s.push_opcode(0xa9); // OP_HASH160
    s.push_data(claim_pubkey_hash).unwrap();
    s.push_opcode(0x88); // OP_EQUALVERIFY
    s.push_opcode(0xac); // OP_CHECKSIG

    // Refund branch
    s.push_opcode(0x67); // OP_ELSE
    s.push_data(&expiry.to_le_bytes()).unwrap();
    s.push_opcode(0xb1); // OP_CHECKLOCKTIMEVERIFY
    s.push_opcode(0x75); // OP_DROP
    s.push_opcode(0x76); // OP_DUP
    s.push_opcode(0xa9); // OP_HASH160
    s.push_data(refund_pubkey_hash).unwrap();
    s.push_opcode(0x88); // OP_EQUALVERIFY
    s.push_opcode(0xac); // OP_CHECKSIG

    s.push_opcode(0x68); // OP_ENDIF
    s
}

/// Build an OP_RETURN data carrier output (unspendable, stores arbitrary data).
///
/// Output: `OP_RETURN <data>` (max 80 bytes of data)
pub fn build_op_return(data: &[u8]) -> Result<Script> {
    if data.len() > 80 {
        return Err(ScriptError::PushTooLarge(data.len()));
    }
    let mut s = Script::new();
    s.push_opcode(0x6a); // OP_RETURN
    if !data.is_empty() {
        s.push_data(data).unwrap();
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_p2pkh() {
        let hash = [0xabu8; 20];
        let script = build_p2pkh(&hash);
        assert_eq!(classify_script(&script), ScriptType::P2PKH);
    }

    #[test]
    fn test_classify_p2sh() {
        let hash = [0x11u8; 20];
        let script = build_p2sh(&hash);
        assert_eq!(classify_script(&script), ScriptType::P2SH);
    }

    #[test]
    fn test_classify_p2ms() {
        let keys = vec![vec![0x02u8; 33], vec![0x02u8; 33], vec![0x02u8; 33]];
        let script = build_p2ms(2, &keys).unwrap();
        assert_eq!(classify_script(&script), ScriptType::P2MS { m: 2, n: 3 });
    }

    #[test]
    fn test_classify_op_return() {
        let script = build_op_return(b"vtorrent").unwrap();
        assert_eq!(classify_script(&script), ScriptType::OpReturn);
    }

    #[test]
    fn test_classify_htlc() {
        let script = build_htlc(&[0u8; 32], &[1u8; 20], &[2u8; 20], 100);
        assert_eq!(classify_script(&script), ScriptType::Htlc);
    }

    #[test]
    fn test_p2pkh_length() {
        let script = build_p2pkh(&[0u8; 20]);
        assert_eq!(script.len(), 25);
    }

    #[test]
    fn test_p2sh_length() {
        let script = build_p2sh(&[0u8; 20]);
        assert_eq!(script.len(), 23);
    }

    #[test]
    fn test_p2ms_m_greater_than_n_fails() {
        let keys = vec![vec![0x02u8; 33]];
        assert!(build_p2ms(2, &keys).is_err());
    }

    #[test]
    fn test_op_return_too_large_fails() {
        let data = vec![0u8; 81];
        assert!(build_op_return(&data).is_err());
    }

    #[test]
    fn test_op_return_empty_ok() {
        let script = build_op_return(&[]).unwrap();
        assert_eq!(classify_script(&script), ScriptType::OpReturn);
    }

    #[test]
    fn test_htlc_structure() {
        let payment_hash = [0xffu8; 32];
        let claim_hash = [0x11u8; 20];
        let refund_hash = [0x22u8; 20];
        let script = build_htlc(&payment_hash, &claim_hash, &refund_hash, 500_000);
        assert!(script.len() > 50); // HTLC scripts are complex
        assert_eq!(classify_script(&script), ScriptType::Htlc);
    }
}
