# Torrent Creator & Publishing — Design

Status: DRAFT (no consensus change; torrent + RPC + UI)
Scope: `vtorrent-torrent` (metainfo build), `vtorrent-rpc`, UI.
Motivation: the client can **consume** torrents (`add_torrent`, magnet parse,
`ut_metadata`) but cannot **create** one — there is no way to seed your own
content. Combined with the on-chain registry
(`docs/torrent-discovery-design.md`), a creator closes the loop: make a torrent,
seed it, announce it, earn VTR. This is the supply side of the whole torrent
economy.

## 1. What exists

- `Metainfo` (`metainfo.rs:6`) with `info_hash`, `pieces: Vec<[u8;20]>`,
  `piece_length`, `files` — the parsed shape.
- SHA1 (`metainfo.rs:3`), `serde_bencode` (with a depth guard,
  `bencode_guard.rs`), magnet parsing.
- **Gap**: no path from a file/directory → `Metainfo` → `.torrent` bytes.

## 2. Design

### 2.1 Create from a file/directory

- Input: a path (file or directory), `piece_length` (auto by size, overridable),
  optional trackers, a private flag, a comment/created-by.
- Walk the path deterministically (sorted), compute the **BEP-3 info dict**:
  - single-file: `name`, `length`, `piece length`, `pieces`.
  - multi-file: `name`, `files: [{length, path[]}]`, `piece length`, `pieces`.
- Hash each piece with **SHA1** (`pieces` = concatenated 20-byte hashes).
- Compute `info_hash = SHA1(bencode(info_dict))` — the info dict only, not the
  whole metainfo.

### 2.2 Encode the `.torrent`

- Bencode the metainfo (`serde_bencode::to_bytes`) — the **encoder** path, which
  the client currently never exercises (it only decodes).
- **Determinism**: byte-identical output for the same input (sorted keys, no
  timestamps in the info dict) so the `info_hash` is stable and reproducible.
- Write the `.torrent` file and/or return the **magnet link**
  (`magnet:?xt=urn:btih:<info_hash>&dn=<name>`).

### 2.3 Seed

- After creating, **add the torrent in seeding mode**: the client already has
  the data on disk, so it verifies existing pieces and seeds (no download).
- Reuse the existing session/engine with a "seed local data" path.

### 2.4 Publish (optional, ties to discovery)

- Optionally **announce** the torrent on-chain
  (`docs/torrent-discovery-design.md`) and/or to the DHT, so others can find it.
- Warn: announcing is public and permanent (privacy).

### 2.5 UI

- A **Create** flow: pick a file/folder, set piece size/trackers, preview the
  file list + size + `info_hash`, then "Create & Seed" and (optionally) "Publish".

## 3. Adversarial review

- **R1 — `info_hash` must be over the info dict only.** Hashing the whole
  metainfo (or including trackers) yields a wrong, non-standard `info_hash` and
  the torrent won't interoperate. Test against a known vector.
- **R2 — determinism.** Non-deterministic encoding (unordered keys, embedded
  timestamps) changes the `info_hash` run-to-run, breaking reproducibility and
  dedup. Sort keys; keep the info dict free of volatile fields.
- **R3 — piece length.** Too small = huge `pieces` string and overhead; too large
  = poor streaming/availability. Auto-select by total size with sane bounds;
  document.
- **R4 — path safety on seed.** Seeding reads local files by the torrent's path
  list; ensure the create path and the seed path agree and are
  traversal-safe (`sanitize_path` exists for disk paths).
- **R5 — publishing is public.** Announcing links the creator's address to the
  content forever; opt-in, fresh address, warn.
- **R6 — large files.** Hashing a multi-GB file must stream (not load into
  memory); bound memory.
- **R7 — no consensus change.** Torrent + RPC + UI only.

## 4. Test plan

- Create from a single file and a directory; the `.torrent` decodes back to the
  same `Metainfo`; `info_hash` matches a known vector.
- Determinism: two runs produce byte-identical `.torrent` and the same
  `info_hash`.
- Seed: adding the created torrent verifies existing pieces and seeds without
  re-downloading.
- Piece-length auto-selection within bounds; large-file hashing streams.
- Publish warns and uses a fresh address.
- No consensus/chain change.

## 5. Non-goals

- Not a web seed / HTTP seeding (a follow-up).
- Not a tracker (the registry + DHT cover discovery).
- Not part of the current soak window.

## 6. References

- `vtorrent-torrent/src/metainfo.rs:3,6,18,84` SHA1, `Metainfo`, `pieces`, hashing.
- `vtorrent-torrent/src/bencode_guard.rs` (bencode depth guard; encoder path).
- `vtorrent-torrent/src/engine.rs` (seed local data), `engine_disk.rs`
  `sanitize_path`.
- `docs/torrent-discovery-design.md` (publish/announce).
