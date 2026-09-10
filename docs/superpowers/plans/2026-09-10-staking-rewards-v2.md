# Staking Rewards v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `GET /api/v1/staking/rewards` + Tauri command + real-data RewardHistory UI.

**Architecture:** New response types in `vtorrent-rpc/src/models.rs`; pure helpers + handler in `vtorrent-rpc/src/handlers/staking.rs`; public route in `vtorrent-rpc/src/server.rs`; Tauri command in `vtorrent-tauri/src/commands/staking.rs`; UI rework in `vtorrent-ui/src/components/staking/RewardHistory.tsx`. Reward = sum of coinstake output values (matches WS event in `vtorrent-node/src/node/staking_loop.rs:154-160`).

**Tech Stack:** Rust (axum Query, tokio Mutex chain), serde, vitest + React (UI).

---

### Task 1: Response models

**Files:**
- Modify: `vtorrent-rpc/src/models.rs` (append after `StakingStatusResponse`, lines 191-200)
- Test: `vtorrent-rpc/src/models.rs` (inline `#[cfg(test)]` — check if file already has a test module; if yes, append there, else create one at file end)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_staking_rewards_response_roundtrip() {
    let resp = StakingRewardsResponse {
        tip_height: 100,
        rewards: vec![StakingRewardItem {
            height: 99,
            timestamp: 1_700_000_000,
            block_hash: "ab".repeat(32),
            reward_sats: 50_000,
            staker_address: Some("VTest".to_string()),
        }],
    };
    let json = serde_json::to_string(&resp).unwrap();
    let back: StakingRewardsResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(back.tip_height, 100);
    assert_eq!(back.rewards.len(), 1);
    assert_eq!(back.rewards[0].reward_sats, 50_000);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p vtorrent-rpc test_staking_rewards_response_roundtrip`
Expected: FAIL with "cannot find type `StakingRewardsResponse`".

- [ ] **Step 3: Write minimal implementation** (append after `StakingStatusResponse` in `vtorrent-rpc/src/models.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingRewardItem {
    pub height: u64,
    pub timestamp: u32,
    pub block_hash: String,
    pub reward_sats: u64,
    pub staker_address: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingRewardsResponse {
    pub tip_height: u64,
    pub rewards: Vec<StakingRewardItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingRewardsQuery {
    pub limit: Option<u64>,
    pub address: Option<String>,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p vtorrent-rpc test_staking_rewards_response_roundtrip`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add vtorrent-rpc/src/models.rs
git commit -m "feat(rpc): staking rewards response models"
```

### Task 2: Pure reward helpers + unit tests

**Files:**
- Modify: `vtorrent-rpc/src/handlers/staking.rs` (append helpers + `#[cfg(test)]` module at file end)
- Test: same file inline module

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod rewards_tests {
    use super::*;

    fn p2pkh_script(hash: &[u8; 20]) -> Vec<u8> {
        let mut s = vec![0x76, 0xa9, 0x14];
        s.extend_from_slice(hash);
        s.push(0x88);
        s.push(0xac);
        s
    }

    #[test]
    fn test_clamp_rewards_limit() {
        assert_eq!(clamp_rewards_limit(None), 20);
        assert_eq!(clamp_rewards_limit(Some(0)), 1);
        assert_eq!(clamp_rewards_limit(Some(5)), 5);
        assert_eq!(clamp_rewards_limit(Some(101)), 100);
    }

    #[test]
    fn test_p2pkh_script_to_address_roundtrip() {
        let hash = [0x11u8; 20];
        let addr = p2pkh_script_to_address(&p2pkh_script(&hash)).unwrap();
        assert!(addr.starts_with('V'));
        assert!(p2pkh_script_to_address(&[0x00, 0x01]).is_none());
        assert!(p2pkh_script_to_address(&vec![0x00; 25]).is_none());
    }

    #[test]
    fn test_coinstake_reward_sums_outputs() {
        let tx = vtorrent_node::block::Transaction {
            version: 1,
            tx_type: vtorrent_node::block::TxType::Coinstake,
            inputs: vec![],
            outputs: vec![
                vtorrent_node::block::TxOutput { value: 0, script_pubkey: vec![] },
                vtorrent_node::block::TxOutput { value: 50_000, script_pubkey: vec![0x76] },
            ],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        assert_eq!(coinstake_reward(&tx), Some(50_000));
        let std_tx = vtorrent_node::block::Transaction {
            version: 1,
            tx_type: vtorrent_node::block::TxType::Standard,
            inputs: vec![],
            outputs: vec![
                vtorrent_node::block::TxOutput { value: 50_000, script_pubkey: vec![0x76] },
            ],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        assert_eq!(coinstake_reward(&std_tx), None);
    }
}
```

Note: construct both transactions literally (no spread — field list above is complete per `vtorrent-node/src/block.rs:46-61`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p vtorrent-rpc rewards_tests`
Expected: FAIL with "cannot find function `clamp_rewards_limit`".

- [ ] **Step 3: Write minimal implementation** (append to `vtorrent-rpc/src/handlers/staking.rs`, above the test module)

```rust
pub(crate) fn clamp_rewards_limit(limit: Option<u64>) -> u64 {
    match limit {
        None => 20,
        Some(n) => n.clamp(1, 100),
    }
}

pub(crate) fn p2pkh_script_to_address(script: &[u8]) -> Option<String> {
    if script.len() != 25
        || script[0] != 0x76
        || script[1] != 0xa9
        || script[2] != 0x14
        || script[23] != 0x88
        || script[24] != 0xac
    {
        return None;
    }
    let addr = vtorrent_core::address::Address::from_hash160(
        &script[3..23],
        vtorrent_core::network::legacy::PUBKEY_ADDRESS_PREFIX,
    )
    .ok()?;
    Some(addr.to_string())
}

pub(crate) fn coinstake_reward(tx: &vtorrent_node::block::Transaction) -> Option<u64> {
    if tx.tx_type != vtorrent_node::block::TxType::Coinstake {
        return None;
    }
    Some(tx.outputs.iter().map(|o| o.value).sum())
}
```

Check imports at top of `handlers/staking.rs`: it already has `use crate::models::*;`. `vtorrent_core` and `vtorrent_node` must be dependencies of `vtorrent-rpc` — verified in `vtorrent-rpc/Cargo.toml` (both present as path deps). `TxType` needs `PartialEq` — verified derived (`#[derive(Debug, Clone, Copy, PartialEq, Eq, ...)]` in `vtorrent-node/src/block.rs:27`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p vtorrent-rpc rewards_tests`
Expected: PASS, 3 passed.

- [ ] **Step 5: Commit**

```bash
git add vtorrent-rpc/src/handlers/staking.rs
git commit -m "feat(rpc): staking reward helpers with tests"
```

### Task 3: Handler + route + integration test

**Files:**
- Modify: `vtorrent-rpc/src/handlers/staking.rs` (add handler)
- Modify: `vtorrent-rpc/src/server.rs` (add route next to line 147 `staking/status`)
- Test: `vtorrent-rpc/src/server/server_tests.rs` (append test near `test_staking_status`, line 448)

- [ ] **Step 1: Write the failing integration test** (append in `server_tests.rs` after `test_staking_status`)

```rust
#[tokio::test]
async fn test_staking_rewards_empty_chain() {
    let app = build_router(AppState::new());
    let (status, body) = get(app, "/api/v1/staking/rewards?limit=5").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["rewards"].as_array().unwrap().is_empty());
}
```

Check the `get` helper signature in `server_tests.rs` (used as `get(app, path)` returning `(StatusCode, Value)` per `test_staking_status`). Query string in path must be supported — if the helper does not accept query strings, pass `"/api/v1/staking/rewards"` without query instead.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p vtorrent-rpc test_staking_rewards_empty_chain`
Expected: FAIL with 404 (no route).

- [ ] **Step 3: Write minimal handler** (append in `handlers/staking.rs`)

```rust
pub async fn get_staking_rewards(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<StakingRewardsQuery>,
) -> RpcResult<Json<StakingRewardsResponse>> {
    let limit = clamp_rewards_limit(query.limit);
    let chain = state.chain.lock().await;
    let tip = chain.best_height();
    let mut rewards = Vec::new();
    let mut height = tip;
    while rewards.len() < limit as usize {
        let Some(block) = chain.get_block_at_height(height) else {
            break;
        };
        if let Some(coinstake) = block
            .transactions
            .iter()
            .find(|tx| tx.tx_type == vtorrent_node::block::TxType::Coinstake)
        {
            let reward_sats: u64 = coinstake.outputs.iter().map(|o| o.value).sum();
            let staker_address = coinstake
                .outputs
                .iter()
                .filter_map(|o| p2pkh_script_to_address(&o.script_pubkey))
                .next();
            if query.address.as_ref().is_none_or(|a| staker_address.as_ref() == Some(a)) {
                let hash = chain
                    .block_hash_at_height(height)
                    .map(hex::encode)
                    .unwrap_or_default();
                rewards.push(StakingRewardItem {
                    height: height as u64,
                    timestamp: block.header.timestamp,
                    block_hash: hash,
                    reward_sats,
                    staker_address,
                });
            }
        }
        if height == 0 {
            break;
        }
        height -= 1;
    }
    Ok(Json(StakingRewardsResponse { tip_height: tip as u64, rewards }))
}
```

`is_none_or` requires Rust 1.82+; check workspace `rust-version` — if older, use `match`/`map_or(true, ...)` instead. `StakingRewardsQuery`/`StakingRewardItem` come via existing `use crate::models::*;`. `hex` is already a dependency (used in `blockchain.rs`).

- [ ] **Step 4: Register the public route** (in `server.rs` public_routes, next to staking/status)

```rust
.route("/api/v1/staking/rewards", get(get_staking_rewards))
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p vtorrent-rpc test_staking_rewards_empty_chain && cargo test -p vtorrent-rpc rewards_tests`
Expected: PASS. Then `cargo clippy -p vtorrent-rpc --all-targets -- -D warnings` exit 0.

- [ ] **Step 6: Commit**

```bash
git add vtorrent-rpc/src/handlers/staking.rs vtorrent-rpc/src/server.rs vtorrent-rpc/src/server/server_tests.rs
git commit -m "feat(rpc): staking rewards history endpoint"
```

### Task 4: Tauri command

**Files:**
- Modify: `vtorrent-tauri/src/commands/staking.rs` (append command)
- Modify: `vtorrent-tauri/src/main.rs` (register in handler list near `get_staking_status`)
- Test: manual via `cargo check -p vtorrent-tauri`; handler logic covered by Task 2-3 helpers (shared scan duplicated minimally — see note)

- [ ] **Step 1: Append the Tauri command** (end of `commands/staking.rs`)

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct StakingRewardEntry {
    pub height: u64,
    pub timestamp: u32,
    pub block_hash: String,
    pub reward_sats: u64,
    pub staker_address: Option<String>,
}

#[tauri::command]
pub async fn get_staking_rewards(
    state: State<'_, AppState>,
    limit: Option<u64>,
    address: Option<String>,
) -> Result<Vec<StakingRewardEntry>> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let limit = limit.unwrap_or(20).clamp(1, 100);
    let chain = handle.rpc_state.chain.lock().await;
    let mut out = Vec::new();
    let mut height = chain.best_height();
    while out.len() < limit as usize {
        let Some(block) = chain.get_block_at_height(height) else {
            break;
        };
        if let Some(coinstake) = block
            .transactions
            .iter()
            .find(|tx| tx.tx_type == vtorrent_node::block::TxType::Coinstake)
        {
            let reward_sats: u64 = coinstake.outputs.iter().map(|o| o.value).sum();
            let staker_address = coinstake
                .outputs
                .iter()
                .filter_map(|o| p2pkh_to_address(&o.script_pubkey))
                .next();
            if address.as_ref().is_none_or(|a| staker_address.as_ref() == Some(a)) {
                out.push(StakingRewardEntry {
                    height: height as u64,
                    timestamp: block.header.timestamp,
                    block_hash: chain
                        .block_hash_at_height(height)
                        .map(hex::encode)
                        .unwrap_or_default(),
                    reward_sats,
                    staker_address,
                });
            }
        }
        if height == 0 {
            break;
        }
        height -= 1;
    }
    Ok(out)
}

fn p2pkh_to_address(script: &[u8]) -> Option<String> {
    if script.len() != 25
        || script[0] != 0x76
        || script[1] != 0xa9
        || script[2] != 0x14
        || script[23] != 0x88
        || script[24] != 0xac
    {
        return None;
    }
    vtorrent_core::address::Address::from_hash160(
        &script[3..23],
        vtorrent_core::network::legacy::PUBKEY_ADDRESS_PREFIX,
    )
    .ok()
    .map(|a| a.to_string())
}
```

Verify before writing: `vtorrent-tauri/Cargo.toml` has `vtorrent-core`, `vtorrent-node`, `hex` deps — if any is missing, add the path/crates version matching the existing entries and note it in the commit. Verify `main.rs` handler list contains `commands::get_staking_status` and add `commands::get_staking_rewards` beside it. Check `is_none_or` MSRV as in Task 3.

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p vtorrent-tauri`
Expected: exit 0.

- [ ] **Step 3: Commit**

```bash
git add vtorrent-tauri/src/commands/staking.rs vtorrent-tauri/src/main.rs vtorrent-tauri/Cargo.toml
git commit -m "feat(tauri): staking rewards history command"
```

(Only include Cargo.toml in the commit if it was actually modified.)

### Task 5: Frontend RewardHistory rework

**Files:**
- Modify: `vtorrent-ui/src/components/staking/RewardHistory.tsx` (rewrite fetch + rows)
- Test: `vtorrent-ui/src/components/staking/RewardHistory.test.ts` (extend with real-average case)
- Modify: `vtorrent-ui/src/pages/StakingPage.tsx` (only if prop plumbing changes — prefer no change)

- [ ] **Step 1: Extend the test with a real-average case** (append to existing file)

```ts
import { dailyAvgReward } from '../../utils/stakingOps'

it('averages non-zero rewards per day', () => {
  const now = 1_700_000_000
  const points = [
    { height: 100, timestamp: now - 3600, rewardSats: 100_000 },
    { height: 101, timestamp: now - 1800, rewardSats: 300_000 },
  ]
  expect(dailyAvgReward(points, now)).toBeCloseTo(400_000 * (86400 / 3600), 0)
})
```

(Merge into the existing `describe` block; the file already imports `dailyAvgReward`.)

- [ ] **Step 2: Run test to verify it passes** (helper already supports it)

Run: `cd vtorrent-ui && pnpm vitest run src/components/staking/RewardHistory.test.ts`
Expected: PASS.

- [ ] **Step 3: Rewrite the fetch to use the endpoint**

```tsx
import { useState } from 'react'
import { isTauri, rpcGet, tauriInvoke } from '../../api'
import { dailyAvgReward, type RewardPoint } from '../../utils/stakingOps'
import { formatVTR } from '../../hooks/useWallet'

interface RewardRow {
  height: number
  timestamp: number
  blockHash: string
  rewardSats: number
  stakerAddress: string | null
}

async function fetchRewards(): Promise<RewardRow[]> {
  if (isTauri()) {
    return tauriInvoke<RewardRow[]>('get_staking_rewards', { limit: 20 })
  }
  const raw = await rpcGet<unknown>('/api/v1/staking/rewards?limit=20')
  const data = camel(raw) as { tipHeight: number, rewards: RewardRow[] }
  return data.rewards
}
```

Replace the component body: `load()` calls `fetchRewards()`, maps rows to `RewardPoint[]` (`{ height, timestamp, rewardSats }`), sets error banner on failure (keep the `loadError` pattern from the v1 fix), renders one row per reward (`height ... time ... value ... address`), and the avg line reverts to `Daily avg: {formatVTR(Math.round(avg))} / day`. Remove `fetchBlock` and the 20-iteration loop. `camel` is already imported in the current file for the old `fetchBlock` — keep that import.

- [ ] **Step 4: Run tests + lint + build**

Run: `cd vtorrent-ui && pnpm vitest run src/components/staking/RewardHistory.test.ts && pnpm lint && pnpm build`
Expected: PASS, lint exit 0, build succeeds.

- [ ] **Step 5: Commit**

```bash
git add vtorrent-ui/src/components/staking/RewardHistory.tsx vtorrent-ui/src/components/staking/RewardHistory.test.ts
git commit -m "feat(ui): real reward history from staking endpoint"
```
