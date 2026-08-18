# Magnet Metadata Fetch (BEP-9/10) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make magnet links downloadable by implementing the BEP-10 extension protocol and BEP-9 `ut_metadata`, so the client fetches the info dict from peers and reconstructs a full `Metainfo`.

**Architecture:** Add `PeerMessage::Extended` to the peer-wire codec and set the extension bit in the handshake. Add a `metadata.rs` module with `ut_metadata` message encode/decode and a `fetch_metadata` function that reassembles the info dict and parses it. Call `fetch_metadata` from `run_engine` when the metainfo has no pieces.

**Tech Stack:** Rust (edition 2021), `serde_bencode`, `tokio`.

**Spec:** `docs/superpowers/specs/2026-08-18-magnet-metadata-design.md`

---

## File Structure

**Modified:**
- `vtorrent-torrent/src/peer_wire.rs` — add `Extended` message + extension bit
- `vtorrent-torrent/src/metadata.rs` (new) — `ut_metadata` messages + `fetch_metadata`
- `vtorrent-torrent/src/lib.rs` — export `metadata`
- `vtorrent-torrent/src/engine.rs` — call `fetch_metadata` in `run_engine`

---

## Task 1: Add `Extended` message to the peer-wire codec

**Files:**
- Modify: `vtorrent-torrent/src/peer_wire.rs`

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `peer_wire.rs`:

```rust
    #[test]
    fn test_encode_decode_extended() {
        let msg = PeerMessage::Extended {
            id: 1,
            payload: vec![0xAB, 0xCD],
        };
        let encoded = msg.encode();
        // length prefix = 1 (id) + 1 (ext id) + 2 (payload) = 4, then id 20
        assert_eq!(encoded[0..5], [0, 0, 0, 4, 20]);
        let (decoded, _) = PeerMessage::decode(&encoded).unwrap().unwrap();
        assert_eq!(
            decoded,
            PeerMessage::Extended {
                id: 1,
                payload: vec![0xAB, 0xCD]
            }
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p vtorrent-torrent test_encode_decode_extended`
Expected: FAIL (compile error — `Extended` variant not defined).

- [ ] **Step 3: Add the `Extended` variant**

In `vtorrent-torrent/src/peer_wire.rs`, add to the `PeerMessage` enum (after `Port`):

```rust
    /// Port — DHT port (BEP-5).
    Port { port: u16 },
    /// Extension protocol message (BEP-10).
    Extended { id: u8, payload: Vec<u8> },
```

- [ ] **Step 4: Add encode support**

In `PeerMessage::encode`, add a match arm (after the `Port` arm):

```rust
            PeerMessage::Extended { id, payload } => {
                let len = (2 + payload.len()) as u32;
                let mut buf = len.to_be_bytes().to_vec();
                buf.push(20);
                buf.push(*id);
                buf.extend_from_slice(payload);
                buf
            }
```

- [ ] **Step 5: Add decode support**

In `PeerMessage::decode`, add a match arm (after the `9 =>` arm, before the `_ =>` arm):

```rust
            20 => {
                if payload.len() < 1 {
                    return Err(TorrentError::PeerWireError(
                        "Extended: payload too short".into(),
                    ));
                }
                PeerMessage::Extended {
                    id: payload[0],
                    payload: payload[1..].to_vec(),
                }
            }
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p vtorrent-torrent test_encode_decode_extended`
Expected: PASS.

- [ ] **Step 7: Run full peer-wire tests and commit**

Run: `cargo test -p vtorrent-torrent peer_wire::`
Expected: all pass.

```bash
git add vtorrent-torrent/src/peer_wire.rs
git commit -m "feat: add Extended message to peer-wire codec"
```

---

## Task 2: Set the extension bit in the handshake

**Files:**
- Modify: `vtorrent-torrent/src/engine.rs`

- [ ] **Step 1: Add a helper for the extension reserved bytes**

In `vtorrent-torrent/src/engine.rs`, add a constant and update `PeerConnection::connect` to set the extension bit. Add near the top of the file (after the imports):

```rust
/// BEP-10 extension bit: reserved[5] bit 0x10.
const EXTENSION_RESERVED: [u8; 8] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00];
```

- [ ] **Step 2: Use the extension reserved bytes in the handshake**

In `PeerConnection::connect`, change the handshake's `reserved` field from `[0u8; 8]` to `EXTENSION_RESERVED`:

```rust
        let handshake = PeerMessage::Handshake {
            info_hash,
            peer_id: our_peer_id,
            reserved: EXTENSION_RESERVED,
        };
```

- [ ] **Step 3: Build and commit**

Run: `cargo build -p vtorrent-torrent 2>&1 | tail -3`
Expected: builds successfully.

```bash
git add vtorrent-torrent/src/engine.rs
git commit -m "feat: advertise BEP-10 extension support in handshake"
```

---

## Task 3: `ut_metadata` message encode/decode

**Files:**
- Create: `vtorrent-torrent/src/metadata.rs`
- Modify: `vtorrent-torrent/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `vtorrent-torrent/src/metadata.rs`:

```rust
//! BEP-9 `ut_metadata` extension: fetch the info dict from peers.

use crate::error::{Result, TorrentError};
use serde_bencode::value::Value;

/// Build the extension handshake dict: `{ "m": { "ut_metadata": id }, "metadata_size": n }`.
pub fn build_extension_handshake(ut_metadata_id: u8, metadata_size: u64) -> Vec<u8> {
    let mut m = std::collections::HashMap::new();
    m.insert(
        b"ut_metadata".to_vec(),
        Value::Int(ut_metadata_id as i64),
    );
    let mut dict = std::collections::HashMap::new();
    dict.insert(b"m".to_vec(), Value::Dict(m));
    dict.insert(b"metadata_size".to_vec(), Value::Int(metadata_size as i64));
    serde_bencode::to_bytes(&Value::Dict(dict)).unwrap_or_default()
}

/// Build a `ut_metadata` request for a piece: `{ "msg_type": 0, "piece": i }`.
pub fn build_request(ut_metadata_id: u8, piece: u32) -> Vec<u8> {
    let mut dict = std::collections::HashMap::new();
    dict.insert(b"msg_type".to_vec(), Value::Int(0));
    dict.insert(b"piece".to_vec(), Value::Int(piece as i64));
    let payload = serde_bencode::to_bytes(&Value::Dict(dict)).unwrap_or_default();
    let mut out = vec![ut_metadata_id];
    out.extend_from_slice(&payload);
    out
}

/// Parse a `ut_metadata` data message, returning (piece_index, total_size, data).
pub fn parse_data(payload: &[u8]) -> Result<(u32, u64, Vec<u8>)> {
    // The payload is: <ut_metadata_id><bencoded dict><piece bytes>.
    // Find the end of the bencoded dict (the first 'e' that closes the top-level dict).
    let mut depth = 0i32;
    let mut dict_end = None;
    for (i, b) in payload.iter().enumerate() {
        match b {
            b'd' => depth += 1,
            b'e' => {
                depth -= 1;
                if depth == 0 {
                    dict_end = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let dict_end = dict_end.ok_or_else(|| TorrentError::PeerWireError("malformed ut_metadata".into()))?;
    let dict_bytes = &payload[1..dict_end];
    let value: Value = serde_bencode::from_bytes(dict_bytes)
        .map_err(|e| TorrentError::PeerWireError(e.to_string()))?;
    let dict = match value {
        Value::Dict(d) => d,
        _ => return Err(TorrentError::PeerWireError("ut_metadata not a dict".into())),
    };
    let piece = match dict.get(&b"piece".to_vec()) {
        Some(Value::Int(i)) => *i as u32,
        _ => return Err(TorrentError::PeerWireError("missing piece".into())),
    };
    let total_size = match dict.get(&b"total_size".to_vec()) {
        Some(Value::Int(i)) => *i as u64,
        _ => return Err(TorrentError::PeerWireError("missing total_size".into())),
    };
    let data = payload[dict_end..].to_vec();
    Ok((piece, total_size, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_handshake_roundtrip() {
        let bytes = build_extension_handshake(3, 1024);
        let value: Value = serde_bencode::from_bytes(&bytes).unwrap();
        if let Value::Dict(d) = value {
            assert!(d.contains_key(&b"m".to_vec()));
            assert!(d.contains_key(&b"metadata_size".to_vec()));
        } else {
            panic!("expected dict");
        }
    }

    #[test]
    fn test_request_has_ut_metadata_id_prefix() {
        let bytes = build_request(3, 0);
        assert_eq!(bytes[0], 3);
        let value: Value = serde_bencode::from_bytes(&bytes[1..]).unwrap();
        if let Value::Dict(d) = value {
            assert_eq!(d.get(&b"msg_type".to_vec()), Some(&Value::Int(0)));
            assert_eq!(d.get(&b"piece".to_vec()), Some(&Value::Int(0)));
        } else {
            panic!("expected dict");
        }
    }

    #[test]
    fn test_parse_data() {
        // Build a data message: id=3, dict {msg_type:1, piece:0, total_size:4}, data "test".
        let mut dict = std::collections::HashMap::new();
        dict.insert(b"msg_type".to_vec(), Value::Int(1));
        dict.insert(b"piece".to_vec(), Value::Int(0));
        dict.insert(b"total_size".to_vec(), Value::Int(4));
        let dict_bytes = serde_bencode::to_bytes(&Value::Dict(dict)).unwrap();
        let mut payload = vec![3u8];
        payload.extend_from_slice(&dict_bytes);
        payload.extend_from_slice(b"test");

        let (piece, total, data) = parse_data(&payload).unwrap();
        assert_eq!(piece, 0);
        assert_eq!(total, 4);
        assert_eq!(data, b"test");
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p vtorrent-torrent metadata::`
Expected: 3 tests pass.

- [ ] **Step 3: Export the module and commit**

In `vtorrent-torrent/src/lib.rs`, add `pub mod metadata;` after `pub mod metainfo;`.

```bash
git add vtorrent-torrent/src/metadata.rs vtorrent-torrent/src/lib.rs
git commit -m "feat: add ut_metadata message encode/decode"
```

---

## Task 4: `fetch_metadata` function

**Files:**
- Modify: `vtorrent-torrent/src/metadata.rs`

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `metadata.rs`:

```rust
    #[test]
    fn test_reassemble_metadata() {
        // A known info dict split into two pieces.
        let info_dict = b"d4:name4:test12:piece lengthi4e6:pieces40:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa4:lengthi8ee";
        let mut pieces = std::collections::HashMap::new();
        pieces.insert(0u32, info_dict[..20].to_vec());
        pieces.insert(1u32, info_dict[20..].to_vec());
        let reassembled = reassemble_metadata(&pieces, info_dict.len() as u64).unwrap();
        assert_eq!(reassembled, info_dict);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p vtorrent-torrent test_reassemble_metadata`
Expected: FAIL (compile error — `reassemble_metadata` not defined).

- [ ] **Step 3: Implement `reassemble_metadata`**

Add to `metadata.rs` (after `parse_data`):

```rust
/// Reassemble the info dict from pieces, in piece order.
pub fn reassemble_metadata(
    pieces: &std::collections::HashMap<u32, Vec<u8>>,
    total_size: u64,
) -> Result<Vec<u8>> {
    let mut indices: Vec<u32> = pieces.keys().copied().collect();
    indices.sort_unstable();
    let mut out = Vec::with_capacity(total_size as usize);
    for i in indices {
        out.extend_from_slice(&pieces[&i]);
    }
    if out.len() as u64 != total_size {
        return Err(TorrentError::PeerWireError(format!(
            "metadata size mismatch: expected {} got {}",
            total_size,
            out.len()
        )));
    }
    Ok(out)
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p vtorrent-torrent test_reassemble_metadata`
Expected: PASS.

- [ ] **Step 5: Run full metadata tests and commit**

Run: `cargo test -p vtorrent-torrent metadata::`
Expected: 4 tests pass.

```bash
git add vtorrent-torrent/src/metadata.rs
git commit -m "feat: add metadata reassembly"
```

---

## Task 5: Wire `fetch_metadata` into `run_engine`

**Files:**
- Modify: `vtorrent-torrent/src/engine.rs`

- [ ] **Step 1: Add a `fetch_metadata` function**

Add to `engine.rs` (after `write_piece_to_disk`):

```rust
/// Fetch the info dict from a peer via BEP-9 `ut_metadata`, returning the
/// parsed `Metainfo`. Returns `None` if the peer does not support extensions.
async fn fetch_metadata_from_peer(
    conn: &mut PeerConnection,
    info_hash: [u8; 20],
    peer_id: [u8; 20],
) -> Option<Metainfo> {
    use crate::metadata;

    // Send the extension handshake (id 0) advertising ut_metadata id 1.
    let handshake = metadata::build_extension_handshake(1, 0);
    let _ = conn
        .send(&PeerMessage::Extended {
            id: 0,
            payload: handshake,
        })
        .await;

    // Read the peer's extension handshake to learn its ut_metadata id and size.
    let mut ut_metadata_id = None;
    let mut metadata_size = 0u64;
    for _ in 0..10 {
        match conn.recv().await {
            Ok(PeerMessage::Extended { id: 0, payload }) => {
                if let Ok(Value::Dict(d)) = serde_bencode::from_bytes::<Value>(&payload) {
                    if let Some(Value::Dict(m)) = d.get(&b"m".to_vec()) {
                        if let Some(Value::Int(id)) = m.get(&b"ut_metadata".to_vec()) {
                            ut_metadata_id = Some(*id as u8);
                        }
                    }
                    if let Some(Value::Int(sz)) = d.get(&b"metadata_size".to_vec()) {
                        metadata_size = *sz as u64;
                    }
                }
                break;
            }
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
    let ut_metadata_id = ut_metadata_id?;
    if metadata_size == 0 {
        return None;
    }

    // Request the metadata in 16 KiB pieces.
    const PIECE_LEN: u64 = 16 * 1024;
    let piece_count = metadata_size.div_ceil(PIECE_LEN);
    let mut pieces: std::collections::HashMap<u32, Vec<u8>> = std::collections::HashMap::new();

    for piece in 0..piece_count as u32 {
        let req = metadata::build_request(ut_metadata_id, piece);
        let _ = conn
            .send(&PeerMessage::Extended {
                id: ut_metadata_id,
                payload: req,
            })
            .await;

        // Read until we get the data for this piece.
        for _ in 0..20 {
            match conn.recv().await {
                Ok(PeerMessage::Extended { id, payload }) if id == ut_metadata_id => {
                    if let Ok((p, _total, data)) = metadata::parse_data(&payload) {
                        pieces.insert(p, data);
                    }
                    break;
                }
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    }

    let info_dict = metadata::reassemble_metadata(&pieces, metadata_size).ok()?;
    Metainfo::from_bytes(&info_dict).ok()
}
```

- [ ] **Step 2: Call `fetch_metadata_from_peer` in `run_engine`**

In `run_engine`, after the announce step and before the download loop, add a metadata-fetch step. Insert after the "Update the session's peer list" block:

```rust
    // If this is a magnet link (no pieces), fetch the info dict from a peer.
    let mut metainfo = metainfo;
    if metainfo.pieces.is_empty() {
        for peer in &peers {
            let addr: SocketAddr = match format!("{}:{}", peer.ip, peer.port).parse() {
                Ok(a) => a,
                Err(_) => continue,
            };
            if let Ok(mut conn) = PeerConnection::connect(addr, metainfo.info_hash, peer_id).await {
                if let Some(full) = fetch_metadata_from_peer(&mut conn, metainfo.info_hash, peer_id).await {
                    metainfo = full;
                    break;
                }
            }
        }
        // Persist the fetched metainfo back into the session.
        {
            let mut guard = sessions.write().await;
            if let Ok(s) = guard.get_session_mut(&session_id) {
                s.metainfo = metainfo.clone();
            }
        }
    }
```

- [ ] **Step 3: Add the `Value` import**

In `engine.rs`, add the import at the top (after the existing `use` lines):

```rust
use serde_bencode::value::Value;
```

- [ ] **Step 4: Build and commit**

Run: `cargo build -p vtorrent-torrent 2>&1 | tail -5`
Expected: builds successfully.

```bash
git add vtorrent-torrent/src/engine.rs
git commit -m "feat: fetch magnet metadata in torrent engine"
```

---

## Final Verification

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace --all-features 2>&1 | rg "test result: FAILED|error\["`
Expected: no failures.

- [ ] **Step 2: Run clippy and fmt**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | rg "warning:|error:"`
Expected: no output.

Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 3: Commit any remaining changes**

```bash
git add -A
git commit -m "chore: final verification of magnet metadata fetch"
```
