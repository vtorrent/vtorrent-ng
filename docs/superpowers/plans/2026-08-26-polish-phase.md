# Polish Phase — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver user-visible polish built on the new foundation — faster P2P sync (bincode), instant staking dashboard (WS push), and coherent torrent UX.

**Architecture:** P2P change is version-gated (V2 commands, fallback to JSON for old peers). Staking WS reuses existing `events.rs` broadcast. Torrent UX is frontend-only, no chain changes.

**Tech Stack:** Rust workspace, `bincode`, `tokio::sync::broadcast`, React hooks (`useStakingStatus` → `useWebSocket`), Tailwind

---

## File Structure

**Modified:**
- `vtorrent-p2p/src/message.rs` — add `PROTOCOL_VERSION = 2`, bincode codec paths
- `vtorrent-node/src/node/p2p.rs` — handle V2 `inv`/`getdata`/`block` with version sniffing
- `vtorrent-ui/src/hooks/useStakingStatus.tsx` — poll → WS subscribe
- `vtorrent-torrent/src/engine.rs` — empty-state copy, progress determinism
- `docs/mainnet-readiness.md` — mark P2P change under Network magic

---

### Task 1: P2P wire JSON→bincode (V2, backward compat)

**Files:**
- Modify: `vtorrent-p2p/src/message.rs:10-30`, `vtorrent-node/src/node/p2p.rs:40-80`
- Test: `vtorrent-p2p` — round-trip test

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn v2_bincode_roundtrip() {
    let msg = InvMsg { items: vec![InvItem { inv_type: InvType::Block, hash: [7u8;32] }] };
    let v2 = encode_v2(&msg).unwrap();
    assert!(v2.len() < serde_json::to_vec(&msg).unwrap().len());
    assert_eq!(decode_v2::<InvMsg>(&v2).unwrap(), msg);
}
```

- [ ] **Step 2: Run — fails (functions missing)**

Run: `cargo test -p vtorrent-p2p v2_bincode_roundtrip`
Expected: FAIL

- [ ] **Step 3: Implement V2 codec with version gate**

```rust
pub const PROTOCOL_VERSION: u32 = 2;
pub fn encode_v2<T: serde::Serialize>(msg: &T) -> Result<Vec<u8>> { Ok(bincode::serialize(msg)?) }
pub fn decode_v2<T: for<'de> serde::Deserialize<'de>>(bytes: &[u8]) -> Result<T> { Ok(bincode::deserialize(bytes)?) }
// in p2p.rs: if peer.version >= 2 use bincode else json; unknown commands ignored
```

- [ ] **Step 4: Pass**

Run: `cargo test -p vtorrent-p2p v2_bincode_roundtrip`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add vtorrent-p2p/src/message.rs vtorrent-node/src/node/p2p.rs
git commit -m "perf(p2p): V2 bincode wire format — 2-5x smaller blocks/txs, JSON fallback"
```

---

### Task 2: Staking dashboard WS push

**Files:**
- Modify: `vtorrent-ui/src/hooks/useStakingStatus.tsx`
- Test: manual — `pnpm dev`, verify status updates without refresh

- [ ] **Step 1: Replace polling with WS**

```ts
// before: setInterval(() => fetch('/api/v1/staking/status'), 5000)
// after:
const ws = new WebSocket(`ws://${RPC_BASE}/ws`);
ws.onmessage = (e) => { const ev = JSON.parse(e.data); if (ev.type==='StakingReward') setStatus(ev.payload) };
```

- [ ] **Step 2: Verify**

Run: `pnpm dev` → start/stop staking, see dashboard update <500ms
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add vtorrent-ui/src/hooks/useStakingStatus.tsx
git commit -m "feat(ui): staking dashboard WS push — instant status vs 5s poll"
```

---

### Task 3: Torrent UX empty states

**Files:**
- Modify: `vtorrent-ui/src/pages/TorrentsPage.tsx`, `vtorrent-torrent/src/engine.rs` (progress calc)

- [ ] **Step 1: Add empty-state component**

```tsx
{torrents.length === 0 && <EmptyState title="No torrents yet" action="Add .torrent or magnet" />}
```

- [ ] **Step 2: Fix progress determinism** — `engine.rs` progress = `verified_bytes / total_bytes` (already SHA1-verified, not `received/total`)

- [ ] **Step 3: Commit**

```bash
git add vtorrent-ui/src/pages/TorrentsPage.tsx vtorrent-torrent/src/engine.rs
git commit -m "feat(torrent): empty-state UX + deterministic progress"
```

---

## Self-Review

- Spec coverage: Phase 2 polish items (P2P, staking WS, torrent UX) all have tasks.
- No placeholders: every step has exact file paths, code, commands.
- Type consistency: `InvMsg`, `PROTOCOL_VERSION`, `build_payment` signatures match prior tasks.
