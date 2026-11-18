//! Standard script type builders and classifier.
//!
//! Provides:
//! - `classify_script()` — identify the type of a scriptPubKey
//! - `build_p2pkh()` — Pay-to-Public-Key-Hash output script
//! - `build_p2sh()` — Pay-to-Script-Hash output script
//! - `build_p2ms()` — M-of-N multisig output script
//! - `build_htlc()` — Hash Time-Locked Contract (atomic swap) script
//! - `build_op_return()` — OP_RETURN data carrier output

use crate::error::{Result, ScriptError};
use crate::script::{Script, ScriptItem};

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
    /// Pay-to-Cold-Stake: a hot staking key can produce blocks (re-locking the
    /// stake to the same script); a cold spending key can spend after a delay.
    P2CS {
        staking_hash: [u8; 20],
        spending_hash: [u8; 20],
        locktime: u32,
    },
    /// OP_RETURN data carrier (unspendable)
    OpReturn,
    /// Unknown or non-standard script
    NonStandard,
}

/// Classify a scriptPubKey into its standard type.
pub fn classify_script(script: &Script) -> ScriptType {
    let b = script.as_bytes();

    // P2PKH: OP_DUP OP_HASH160 <20> OP_EQUALVERIFY OP_CHECKSIG
    if b.len() == 25
        && b[0] == 0x76
        && b[1] == 0xa9
        && b[2] == 0x14
        && b[23] == 0x88
        && b[24] == 0xac
    {
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
                if (0x51..=0x60).contains(&n_byte) {
                    let n = n_byte - 0x50;
                    if m <= n {
                        return ScriptType::P2MS { m, n };
                    }
                }
            }
        }
    }

    // P2CS: OP_IF <push20 staking> OP_CHECKSIGVERIFY OP_ELSE <push4 locktime>
    //       OP_CHECKLOCKTIMEVERIFY OP_DROP <push20 spending> OP_CHECKSIGVERIFY OP_ENDIF
    // Exact-shape match so it cannot be confused with the HTLC (which starts
    // OP_IF OP_SHA256).
    if let Some((staking_hash, spending_hash, locktime)) = parse_p2cs(b) {
        return ScriptType::P2CS {
            staking_hash,
            spending_hash,
            locktime,
        };
    }

    // HTLC: OP_IF OP_SHA256 <32-byte hash> OP_EQUALVERIFY ... (standard
    // builder) or OP_IF OP_SIZE <32> OP_EQUALVERIFY OP_SHA256 <32> ...
    // (atomic-swap builder). Match the prefix and the hash push, not just the
    // first two opcodes.
    let direct = b.len() >= 36 && b[0] == 0x63 && b[1] == 0xa8 && b[2] == 0x20 && b[35] == 0x88;
    let sized = b.len() >= 40
        && b[0] == 0x63
        && b[1] == 0x82
        && b[2] == 0x01
        && b[3] == 0x20
        && b[4] == 0x88
        && b[5] == 0xa8
        && b[6] == 0x20
        && b[39] == 0x88;
    if direct || sized {
        return ScriptType::Htlc;
    }

    ScriptType::NonStandard
}

/// Parse the exact P2CS script shape, returning `(staking_hash, spending_hash,
/// locktime)` if it matches.
fn parse_p2cs(b: &[u8]) -> Option<([u8; 20], [u8; 20], u32)> {
    let mut cursor = 0usize;
    let expect = |cursor: &mut usize, opcode: u8| -> bool {
        if b.get(*cursor) == Some(&opcode) {
            *cursor += 1;
            true
        } else {
            false
        }
    };
    let push20 = |cursor: &mut usize| -> Option<[u8; 20]> {
        if b.get(*cursor) != Some(&0x14) {
            return None;
        }
        let bytes = b.get(*cursor + 1..*cursor + 21)?;
        *cursor += 21;
        bytes.try_into().ok()
    };

    if !expect(&mut cursor, 0x63) || !expect(&mut cursor, 0x76) || !expect(&mut cursor, 0xa9) {
        return None;
    }
    let staking = push20(&mut cursor)?;
    if !expect(&mut cursor, 0x88) || !expect(&mut cursor, 0xac) || !expect(&mut cursor, 0x67) {
        return None;
    }
    let (locktime, consumed) = read_script_num(&b[cursor..])?;
    cursor += consumed;
    if !expect(&mut cursor, 0xb1)
        || !expect(&mut cursor, 0x75)
        || !expect(&mut cursor, 0x76)
        || !expect(&mut cursor, 0xa9)
    {
        return None;
    }
    let spending = push20(&mut cursor)?;
    if !expect(&mut cursor, 0x88)
        || !expect(&mut cursor, 0xac)
        || !expect(&mut cursor, 0x68)
        || cursor != b.len()
    {
        return None;
    }
    Some((staking, spending, locktime))
}

/// Read a non-negative script number and return `(value, bytes consumed)`.
///
/// Accepts `OP_0`, `OP_1..OP_16`, and `PUSH1..PUSH5`. Non-minimal pushes are
/// accepted so scripts created by older builds still classify.
fn read_script_num(b: &[u8]) -> Option<(u32, usize)> {
    let opcode = *b.first()?;
    if opcode == 0x00 {
        return Some((0, 1));
    }
    if (0x51..=0x60).contains(&opcode) {
        return Some((u32::from(opcode - 0x50), 1));
    }
    let len = match opcode {
        0x01..=0x05 => opcode as usize,
        _ => return None,
    };
    let data = b.get(1..1 + len)?;
    if data.last()? & 0x80 != 0 {
        return None;
    }
    let mut value = 0u64;
    for (index, byte) in data.iter().enumerate() {
        value |= u64::from(*byte) << (8 * index);
    }
    Some((u32::try_from(value).ok()?, 1 + len))
}

/// Build a P2PKH scriptPubKey from a 20-byte public key hash.
///
/// Output: `OP_DUP OP_HASH160 <hash> OP_EQUALVERIFY OP_CHECKSIG`
pub fn build_p2pkh(pubkey_hash: &[u8; 20]) -> Result<Script> {
    let mut s = Script::new();
    s.push_opcode(0x76); // OP_DUP
    s.push_opcode(0xa9); // OP_HASH160
    s.push_data(pubkey_hash)
        .map_err(|e| ScriptError::Serialization(e.to_string()))?;
    s.push_opcode(0x88); // OP_EQUALVERIFY
    s.push_opcode(0xac); // OP_CHECKSIG
    Ok(s)
}

/// Build a P2SH scriptPubKey from a 20-byte redeem script hash.
///
/// Output: `OP_HASH160 <hash> OP_EQUAL`
pub fn build_p2sh(script_hash: &[u8; 20]) -> Result<Script> {
    let mut s = Script::new();
    s.push_opcode(0xa9); // OP_HASH160
    s.push_data(script_hash)
        .map_err(|e| ScriptError::Serialization(e.to_string()))?;
    s.push_opcode(0x87); // OP_EQUAL
    Ok(s)
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
        if key.len() > 520 {
            return Err(ScriptError::PushTooLarge(key.len()));
        }
        s.push_data(key)
            .map_err(|e| ScriptError::Serialization(e.to_string()))?;
    }
    s.push_int(n);
    s.push_opcode(0xae); // OP_CHECKMULTISIG
    Ok(s)
}

/// Push a non-negative script number in minimal encoding.
///
/// Bitcoin's `CScriptNum` rejects non-minimal encodings when the minimal-data
/// rule is active. The old builders pushed a fixed four-byte little-endian
/// value for CLTV, which works today but would make every P2CS/HTLC spend
/// unspendable under a stricter script-version rule.
fn push_script_num(s: &mut Script, n: u32) -> Result<()> {
    if n == 0 {
        s.push_opcode(0x00); // OP_0
        return Ok(());
    }
    if n <= 16 {
        s.push_opcode(0x50 + n as u8); // OP_1 .. OP_16
        return Ok(());
    }
    let mut bytes = n.to_le_bytes().to_vec();
    while bytes.len() > 1 && bytes.last() == Some(&0) && bytes[bytes.len() - 2] & 0x80 == 0 {
        bytes.pop();
    }
    if bytes.last().is_some_and(|byte| byte & 0x80 != 0) {
        bytes.push(0);
    }
    s.push_data(&bytes)
        .map_err(|e| ScriptError::Serialization(e.to_string()))
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
) -> Result<Script> {
    let mut s = Script::new();

    // Claim branch
    s.push_opcode(0x63); // OP_IF
    s.push_opcode(0xa8); // OP_SHA256
    s.push_data(payment_hash)
        .map_err(|e| ScriptError::Serialization(e.to_string()))?;
    s.push_opcode(0x88); // OP_EQUALVERIFY
    s.push_opcode(0x76); // OP_DUP
    s.push_opcode(0xa9); // OP_HASH160
    s.push_data(claim_pubkey_hash)
        .map_err(|e| ScriptError::Serialization(e.to_string()))?;
    s.push_opcode(0x88); // OP_EQUALVERIFY
    s.push_opcode(0xac); // OP_CHECKSIG

    // Refund branch
    s.push_opcode(0x67); // OP_ELSE
    push_script_num(&mut s, expiry)?;
    s.push_opcode(0xb1); // OP_CHECKLOCKTIMEVERIFY
    s.push_opcode(0x75); // OP_DROP
    s.push_opcode(0x76); // OP_DUP
    s.push_opcode(0xa9); // OP_HASH160
    s.push_data(refund_pubkey_hash)
        .map_err(|e| ScriptError::Serialization(e.to_string()))?;
    s.push_opcode(0x88); // OP_EQUALVERIFY
    s.push_opcode(0xac); // OP_CHECKSIG

    s.push_opcode(0x68); // OP_ENDIF
    Ok(s)
}

/// Build a Pay-to-Cold-Stake (P2CS) scriptPubKey.
///
/// ```text
/// OP_IF
///   <staking_pubkey_hash> OP_CHECKSIGVERIFY
/// OP_ELSE
///   <locktime> OP_CHECKLOCKTIMEVERIFY OP_DROP
///   <spending_pubkey_hash> OP_CHECKSIGVERIFY
/// OP_ENDIF
/// ```
///
/// The staking key (hot) can only take the `OP_IF` branch, which the coinstake
/// rules force to re-lock the stake to the same script; the spending key (cold)
/// takes the `OP_ELSE` branch after `locktime`. See
/// `docs/cold-staking-p2cs-design.md`.
pub fn build_p2cs(
    staking_pubkey_hash: &[u8; 20],
    spending_pubkey_hash: &[u8; 20],
    locktime: u32,
) -> Result<Script> {
    let mut s = Script::new();
    s.push_opcode(0x63); // OP_IF
    s.push_opcode(0x76); // OP_DUP
    s.push_opcode(0xa9); // OP_HASH160
    s.push_data(staking_pubkey_hash)
        .map_err(|e| ScriptError::Serialization(e.to_string()))?;
    s.push_opcode(0x88); // OP_EQUALVERIFY
    s.push_opcode(0xac); // OP_CHECKSIG (leaves true)
    s.push_opcode(0x67); // OP_ELSE
    push_script_num(&mut s, locktime)?;
    s.push_opcode(0xb1); // OP_CHECKLOCKTIMEVERIFY
    s.push_opcode(0x75); // OP_DROP
    s.push_opcode(0x76); // OP_DUP
    s.push_opcode(0xa9); // OP_HASH160
    s.push_data(spending_pubkey_hash)
        .map_err(|e| ScriptError::Serialization(e.to_string()))?;
    s.push_opcode(0x88); // OP_EQUALVERIFY
    s.push_opcode(0xac); // OP_CHECKSIG (leaves true)
    s.push_opcode(0x68); // OP_ENDIF
    Ok(s)
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

/// Count signature operations in a script, matching the legacy client's
/// `CScript::GetSigOpCount`.
///
/// `accurate` controls whether `OP_CHECKMULTISIG` uses the preceding
/// `OP_1`..`OP_16` as the key count or assumes the legacy maximum of 20.
pub fn count_sigops(script: &Script, accurate: bool) -> usize {
    count_sigops_raw(script.as_bytes(), accurate)
}

/// Count signature operations directly from raw script bytes.
///
/// Unlike [`count_sigops`], this does not reject scripts above the execution
/// size cap: the legacy client counts sigops before that check, so an oversized
/// output script must still contribute to the block limit.
pub fn count_sigops_raw(script: &[u8], accurate: bool) -> usize {
    let mut count = 0usize;
    let mut last_opcode = None;
    let mut index = 0usize;
    while index < script.len() {
        let opcode = script[index];
        index += 1;
        let push_len = match opcode {
            0..=75 => Some(opcode as usize),
            0x4c => {
                let Some(len) = script.get(index) else { break };
                index += 1;
                Some(*len as usize)
            }
            0x4d => {
                let Some(bytes) = script.get(index..index + 2) else {
                    break;
                };
                index += 2;
                Some(u16::from_le_bytes([bytes[0], bytes[1]]) as usize)
            }
            0x4e => {
                let Some(bytes) = script.get(index..index + 4) else {
                    break;
                };
                index += 4;
                Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize)
            }
            _ => None,
        };
        if let Some(len) = push_len {
            if index.saturating_add(len) > script.len() {
                break;
            }
            index += len;
            last_opcode = Some(0);
            continue;
        }
        match opcode {
            0xac | 0xad => count += 1,
            0xae | 0xaf => {
                if accurate {
                    if let Some(op @ 0x51..=0x60) = last_opcode {
                        count += (op - 0x50) as usize;
                    } else {
                        count += 20;
                    }
                } else {
                    count += 20;
                }
            }
            _ => {}
        }
        last_opcode = Some(opcode);
    }
    count
}

/// Count the signature operations a P2SH spend contributes.
///
/// Mirrors the legacy `CScript::GetSigOpCount(scriptSig)`: the scriptSig must
/// contain only pushes and small integers, and its last pushed item is the
/// redeem script.
pub fn p2sh_sigops(script_pubkey: &Script, script_sig: &Script) -> usize {
    if classify_script(script_pubkey) != ScriptType::P2SH {
        return count_sigops(script_pubkey, true);
    }
    let mut redeem: &[u8] = &[];
    for item in script_sig.iter() {
        match item {
            ScriptItem::PushData(data) => redeem = data,
            ScriptItem::Opcode(0x4f..=0x60) => redeem = &[],
            ScriptItem::Opcode(_) => return 0,
        }
    }
    Script::from_bytes(redeem.to_vec())
        .map(|script| count_sigops(&script, true))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_p2pkh() {
        let hash = [0xabu8; 20];
        let script = build_p2pkh(&hash).unwrap();
        assert_eq!(classify_script(&script), ScriptType::P2PKH);
    }

    #[test]
    fn test_classify_p2sh() {
        let hash = [0x11u8; 20];
        let script = build_p2sh(&hash).unwrap();
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
        let script = build_htlc(&[0u8; 32], &[1u8; 20], &[2u8; 20], 100).unwrap();
        assert_eq!(classify_script(&script), ScriptType::Htlc);
    }

    #[test]
    fn test_p2pkh_length() {
        let script = build_p2pkh(&[0u8; 20]).unwrap();
        assert_eq!(script.len(), 25);
    }

    #[test]
    fn test_p2sh_length() {
        let script = build_p2sh(&[0u8; 20]).unwrap();
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
        let script = build_htlc(&payment_hash, &claim_hash, &refund_hash, 500_000).unwrap();
        assert!(script.len() > 50); // HTLC scripts are complex
        assert_eq!(classify_script(&script), ScriptType::Htlc);
    }
    #[test]
    fn script_numbers_are_minimal_and_legacy_pushes_still_parse() {
        assert_eq!(read_script_num(&[0x00]), Some((0, 1)));
        assert_eq!(read_script_num(&[0x51]), Some((1, 1)));
        // Legacy fixed PUSH4 zero is still accepted.
        assert_eq!(read_script_num(&[0x04, 0, 0, 0, 0]), Some((0, 5)));
        // Minimal PUSH5 for u32::MAX (sign byte appended).
        assert_eq!(
            read_script_num(&[0x05, 0xff, 0xff, 0xff, 0xff, 0x00]),
            Some((u32::MAX, 6))
        );
    }

    #[test]
    fn test_p2cs_zero_locktime_is_minimal() {
        let script = build_p2cs(&[0x11; 20], &[0x22; 20], 0).unwrap();
        assert!(script.len() < 60);
        match classify_script(&script) {
            ScriptType::P2CS { locktime, .. } => assert_eq!(locktime, 0),
            other => panic!("expected P2CS, got {other:?}"),
        }
    }

    #[test]
    fn test_p2cs_roundtrip() {
        let staking = [0x11u8; 20];
        let spending = [0x22u8; 20];
        let script = build_p2cs(&staking, &spending, 1_700_000_000).unwrap();
        match classify_script(&script) {
            ScriptType::P2CS {
                staking_hash,
                spending_hash,
                locktime,
            } => {
                assert_eq!(staking_hash, staking);
                assert_eq!(spending_hash, spending);
                assert_eq!(locktime, 1_700_000_000);
            }
            other => panic!("expected P2CS, got {other:?}"),
        }
    }

    #[test]
    fn test_p2cs_not_confused_with_htlc() {
        // An HTLC (OP_IF OP_SHA256 ...) must still classify as Htlc, not P2CS.
        let htlc = build_htlc(&[0u8; 32], &[1u8; 20], &[2u8; 20], 100).unwrap();
        assert_eq!(classify_script(&htlc), ScriptType::Htlc);
    }

    #[test]
    fn classifies_the_atomic_swap_htlc_shape() {
        let mut bytes = vec![0x63, 0x82, 0x01, 0x20, 0x88, 0xa8, 0x20];
        bytes.extend([0u8; 32]);
        bytes.push(0x88);
        bytes.extend([0x76, 0xa9, 0x14]);
        bytes.extend([0u8; 20]);
        bytes.extend([
            0x88, 0xac, 0x67, 0x04, 0, 0, 0, 0, 0xb1, 0x75, 0x76, 0xa9, 0x14,
        ]);
        bytes.extend([0u8; 20]);
        bytes.extend([0x88, 0xac, 0x68]);
        let script = Script::from_bytes(bytes).unwrap();
        assert_eq!(classify_script(&script), ScriptType::Htlc);
    }

    #[test]
    fn counts_checksig_and_multisig_sigops() {
        let p2pkh = build_p2pkh(&[0x11; 20]).unwrap();
        assert_eq!(count_sigops(&p2pkh, false), 1);

        let keys = vec![vec![0x02; 33], vec![0x03; 33], vec![0x04; 33]];
        let multisig = build_p2ms(2, &keys).unwrap();
        assert_eq!(count_sigops(&multisig, true), 3);
        assert_eq!(count_sigops(&multisig, false), 20);

        assert_eq!(count_sigops_raw(&vec![0xac; 20_001], false), 20_001);
    }

    #[test]
    fn p2sh_sigop_count_uses_the_pushed_redeem_script() {
        let keys = vec![vec![0x02; 33], vec![0x03; 33], vec![0x04; 33]];
        let redeem = build_p2ms(2, &keys).unwrap();
        let p2sh = build_p2sh(&[0x22; 20]).unwrap();

        let mut script_sig = Script::new();
        script_sig.push_opcode(0x00);
        script_sig.push_data(redeem.as_bytes()).unwrap();
        assert_eq!(p2sh_sigops(&p2sh, &script_sig), 3);

        let mut no_redeem = Script::new();
        no_redeem.push_opcode(0x51);
        assert_eq!(p2sh_sigops(&p2sh, &no_redeem), 0);

        let mut non_push = Script::new();
        non_push.push_opcode(0x76);
        assert_eq!(p2sh_sigops(&p2sh, &non_push), 0);
    }
}
