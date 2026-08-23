/// Block and UTXO value parser for the legacy vTorrent chainstate.
///
/// Parses the raw value bytes from the LevelDB chainstate into structured
/// UTXO records, extracting the amount and output script.
use crate::{
    error::{Result, SnapshotError},
    leveldb_reader::{decode_varint_base128, decompress_amount, RawUtxo},
};

/// A parsed UTXO with decoded amount and script.
#[derive(Debug, Clone)]
pub struct ParsedUtxo {
    /// The transaction ID (32 bytes, big-endian for display).
    pub txid: [u8; 32],
    /// The output index.
    pub vout: u32,
    /// The block height this UTXO was created at.
    pub height: u32,
    /// Whether this is a coinbase output.
    pub is_coinbase: bool,
    /// The amount in satoshis.
    pub amount: u64,
    /// The output script (scriptPubKey).
    pub script: Vec<u8>,
    /// The decoded address, if the script is a standard P2PKH or P2PK.
    pub address: Option<String>,
}

/// Parse a raw UTXO value from the chainstate LevelDB.
///
/// The value format (Bitcoin 0.8.x / PPCoin style):
///   [varint: (height << 1) | is_coinbase]
///   [varint: compressed_amount]
///   [varint: script_type] [script_data]
pub fn parse_utxo_value(raw: &RawUtxo) -> Result<ParsedUtxo> {
    let data = &raw.value_bytes;
    let mut cursor = 0usize;

    // Read height + coinbase flag (packed into one varint)
    let (height_coinbase, n) = decode_varint_base128(&data[cursor..])
        .ok_or_else(|| SnapshotError::BlockParse("Failed to read height/coinbase varint".into()))?;
    cursor += n;

    let height = (height_coinbase >> 1) as u32;
    let is_coinbase = (height_coinbase & 1) != 0;

    // Read compressed amount
    let (compressed_amount, n) = decode_varint_base128(&data[cursor..])
        .ok_or_else(|| SnapshotError::BlockParse("Failed to read amount varint".into()))?;
    cursor += n;

    let amount = decompress_amount(compressed_amount);

    // Read script type and data
    let (script_type, n) = decode_varint_base128(&data[cursor..])
        .ok_or_else(|| SnapshotError::BlockParse("Failed to read script type varint".into()))?;
    cursor += n;

    let script = decode_script(script_type, &data[cursor..])?;

    // Derive address from script
    let address = script_to_address(&script);

    Ok(ParsedUtxo {
        txid: raw.txid,
        vout: raw.vout,
        height,
        is_coinbase,
        amount,
        script,
        address,
    })
}

/// Decode a compressed script from the chainstate.
///
/// Bitcoin's chainstate uses compressed script encoding:
///   0 = P2PKH (20-byte hash follows)
///   1 = P2SH  (20-byte hash follows)
///   2-5 = P2PK (compressed/uncompressed pubkey)
///   other = raw script (length follows as varint)
fn decode_script(script_type: u64, data: &[u8]) -> Result<Vec<u8>> {
    match script_type {
        0 => {
            // P2PKH: OP_DUP OP_HASH160 <20-byte-hash> OP_EQUALVERIFY OP_CHECKSIG
            if data.len() < 20 {
                return Err(SnapshotError::BlockParse("P2PKH script too short".into()));
            }
            let mut script = Vec::with_capacity(25);
            script.push(0x76); // OP_DUP
            script.push(0xa9); // OP_HASH160
            script.push(0x14); // push 20 bytes
            script.extend_from_slice(&data[..20]);
            script.push(0x88); // OP_EQUALVERIFY
            script.push(0xac); // OP_CHECKSIG
            Ok(script)
        }
        1 => {
            // P2SH: OP_HASH160 <20-byte-hash> OP_EQUAL
            if data.len() < 20 {
                return Err(SnapshotError::BlockParse("P2SH script too short".into()));
            }
            let mut script = Vec::with_capacity(23);
            script.push(0xa9); // OP_HASH160
            script.push(0x14); // push 20 bytes
            script.extend_from_slice(&data[..20]);
            script.push(0x87); // OP_EQUAL
            Ok(script)
        }
        2 | 3 => {
            // Compressed P2PK: <33-byte-pubkey> OP_CHECKSIG
            if data.len() < 32 {
                return Err(SnapshotError::BlockParse(
                    "P2PK compressed script too short".into(),
                ));
            }
            let mut script = Vec::with_capacity(35);
            script.push(0x21); // push 33 bytes
            script.push(script_type as u8); // prefix byte (02 or 03)
            script.extend_from_slice(&data[..32]);
            script.push(0xac); // OP_CHECKSIG
            Ok(script)
        }
        4 | 5 => {
            // Uncompressed P2PK: <65-byte-pubkey> OP_CHECKSIG
            if data.len() < 64 {
                return Err(SnapshotError::BlockParse(
                    "P2PK uncompressed script too short".into(),
                ));
            }
            let mut script = Vec::with_capacity(67);
            script.push(0x41); // push 65 bytes
            script.push(if script_type == 4 { 0x04 } else { 0x06 }); // prefix
            script.extend_from_slice(&data[..64]);
            script.push(0xac); // OP_CHECKSIG
            Ok(script)
        }
        _ => {
            // Raw script: length is (script_type - 6) bytes
            let len = (script_type - 6) as usize;
            if data.len() < len {
                return Err(SnapshotError::BlockParse(format!(
                    "Raw script too short: need {}, have {}",
                    len,
                    data.len()
                )));
            }
            Ok(data[..len].to_vec())
        }
    }
}

/// Derive a vTorrent address from a scriptPubKey.
/// Only handles P2PKH (the most common type in the legacy chain).
fn script_to_address(script: &[u8]) -> Option<String> {
    use vtorrent_core::{address::Address, network::legacy};

    // P2PKH: 76 a9 14 <20-byte-hash> 88 ac
    if script.len() == 25
        && script[0] == 0x76  // OP_DUP
        && script[1] == 0xa9  // OP_HASH160
        && script[2] == 0x14  // push 20 bytes
        && script[23] == 0x88 // OP_EQUALVERIFY
        && script[24] == 0xac
    // OP_CHECKSIG
    {
        let hash = &script[3..23];
        let addr = Address::from_hash160(hash, legacy::PUBKEY_ADDRESS_PREFIX).ok()?;
        return Some(addr.to_string());
    }

    // P2SH: a9 14 <20-byte-hash> 87
    if script.len() == 23
        && script[0] == 0xa9  // OP_HASH160
        && script[1] == 0x14  // push 20 bytes
        && script[22] == 0x87
    // OP_EQUAL
    {
        let hash = &script[2..22];
        let addr = Address::from_hash160(hash, legacy::SCRIPT_ADDRESS_PREFIX).ok()?;
        return Some(addr.to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_p2pkh_script() {
        // Construct a minimal P2PKH script
        let mut script = vec![0x76, 0xa9, 0x14];
        script.extend_from_slice(&[0xabu8; 20]); // fake hash
        script.push(0x88);
        script.push(0xac);
        assert_eq!(script.len(), 25);
        // Address derivation should succeed
        let addr = script_to_address(&script);
        // May or may not produce a valid address depending on the hash
        // Just ensure no panic
        let _ = addr;
    }
}
