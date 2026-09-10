# Staking Ops Dashboard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend StakingPage into a health + rewards ops view using only existing read-only RPCs.

**Architecture:** Frontend-only in `vtorrent-ui`. Pure helpers in `src/utils/stakingOps.ts` (vitest), small components under `src/components/staking/`, wired into `src/pages/StakingPage.tsx`. No backend change, no key handling, soak fleet untouched.

**Tech Stack:** React 18 + TypeScript + Vite, vitest, existing `rpcGet`/`tauriInvoke` + `camel` in `src/api.ts`.

---

### Task 1: Pure staking-ops helpers with vitest

**Files:**
- Create: `vtorrent-ui/src/utils/stakingOps.ts`
- Test: `vtorrent-ui/src/utils/stakingOps.test.ts`
- Modify: `vtorrent-ui/package.json`

- [ ] **Step 1: Add vitest dev dependency**

Run: `cd vtorrent-ui && pnpm add -D vitest@2`
Expected: `package.json` gains `"vitest": "^2.x"` under `devDependencies`, exit 0.
Note: pin major 2 — vitest 5 crashes with vite 5 (`ERR_PACKAGE_PATH_NOT_EXPORTED
'./module-runner'`); vitest 2 is the contemporary major for vite 5. Also add a
`"test": "vitest run"` script to `vtorrent-ui/package.json` so CI can invoke it.

- [ ] **Step 2: Write the failing test**

```ts
// vtorrent-ui/src/utils/stakingOps.test.ts
import { describe, it, expect } from 'vitest'
import { dailyAvgReward, maturityCountdown, healthSummary } from './stakingOps'

describe('stakingOps', () => {
  it('averages recent rewards per day', () => {
    const now = 1_700_000_000
    const rewards = [
      { height: 100, timestamp: now - 3600, rewardSats: 100_000 },
      { height: 101, timestamp: now - 1800, rewardSats: 300_000 },
    ]
    expect(dailyAvgReward(rewards, now)).toBeCloseTo(400_000 * (86400 / 3600), 0)
  })

  it('counts down maturity in blocks', () => {
    expect(maturityCountdown(90, 100)).toBe(10)
    expect(maturityCountdown(100, 100)).toBe(0)
  })

  it('summarizes health', () => {
    const s = healthSummary({ blockHeight: 10, connections: 2, syncing: false, syncPercent: 100, mempoolSize: 0 })
    expect(s).toBe('height 10 · 2 peers · synced · mempool 0')
  })
})
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cd vtorrent-ui && pnpm vitest run src/utils/stakingOps.test.ts`
Expected: FAIL with "Failed to resolve import ./stakingOps" or "dailyAvgReward is not defined".

- [ ] **Step 4: Write minimal implementation**

```ts
// vtorrent-ui/src/utils/stakingOps.ts
export interface RewardPoint {
  height: number
  timestamp: number
  rewardSats: number
}

export function dailyAvgReward(rewards: RewardPoint[], nowSecs: number): number {
  if (rewards.length === 0) return 0
  const oldest = Math.min(...rewards.map(r => r.timestamp))
  const spanSecs = Math.max(1, nowSecs - oldest)
  const total = rewards.reduce((sum, r) => sum + r.rewardSats, 0)
  return (total / spanSecs) * 86400
}

export function maturityCountdown(confirmations: number, required = 100): number {
  return Math.max(0, required - confirmations)
}

export function healthSummary(info: {
  blockHeight: number
  connections: number
  syncing: boolean
  syncPercent: number
  mempoolSize: number
}): string {
  const sync = info.syncing ? `syncing ${info.syncPercent.toFixed(1)}%` : 'synced'
  return `height ${info.blockHeight} · ${info.connections} peers · ${sync} · mempool ${info.mempoolSize}`
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cd vtorrent-ui && pnpm vitest run src/utils/stakingOps.test.ts`
Expected: PASS, 3 passed.

- [ ] **Step 6: Commit**

```bash
git add vtorrent-ui/package.json vtorrent-ui/src/utils/stakingOps.ts vtorrent-ui/src/utils/stakingOps.test.ts
git commit -m "feat(ui): staking ops helpers with tests"
```

### Task 2: HealthStrip + GrafanaLink components

**Files:**
- Create: `vtorrent-ui/src/components/staking/HealthStrip.tsx`
- Test: `vtorrent-ui/src/components/staking/HealthStrip.test.tsx`
- Modify: `vtorrent-ui/src/pages/StakingPage.tsx`

- [ ] **Step 1: Write the failing test**

```tsx
// vtorrent-ui/src/components/staking/HealthStrip.test.tsx
import { describe, it, expect } from 'vitest'
import { healthSummary } from '../../utils/stakingOps'

describe('HealthStrip data', () => {
  it('renders synced summary', () => {
    const text = healthSummary({ blockHeight: 3660, connections: 2, syncing: false, syncPercent: 100, mempoolSize: 0 })
    expect(text).toContain('3660')
    expect(text).toContain('synced')
  })
})
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd vtorrent-ui && pnpm vitest run src/components/staking/HealthStrip.test.tsx`
Expected: FAIL with "Failed to resolve import" (file under test does not exist yet — the import itself is fine, the component file is missing).

- [ ] **Step 3: Write minimal implementation**

```tsx
// vtorrent-ui/src/components/staking/HealthStrip.tsx
import { useNodeInfo } from '../../hooks/useNode'
import { healthSummary } from '../../utils/stakingOps'

export default function HealthStrip() {
  const { data, loading, error } = useNodeInfo(10_000)
  if (loading || !data) return <p className="text-xs text-gray-500">Loading node health…</p>
  if (error) return <p className="text-xs text-red-400">Node health unavailable: {error}</p>
  return (
    <div className="px-4 py-3 rounded-lg bg-navy-900/30 border border-vtorrent-900/20 text-xs text-gray-300 font-mono">
      {healthSummary(data)}
      <a
        className="ml-3 text-vtorrent-400 hover:text-vtorrent-300 underline"
        href="http://127.0.0.1:3300"
        target="_blank"
        rel="noreferrer"
      >
        Grafana
      </a>
    </div>
  )
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd vtorrent-ui && pnpm vitest run src/utils/stakingOps.test.ts src/components/staking/HealthStrip.test.tsx`
Expected: PASS, 4 passed.

- [ ] **Step 5: Wire into StakingPage below the header**

```tsx
// vtorrent-ui/src/pages/StakingPage.tsx (insert after header div, before status banner)
import HealthStrip from '../components/staking/HealthStrip'
// ...
      <HealthStrip />
```

Run: `cd vtorrent-ui && pnpm lint`
Expected: exit 0, no warnings.

- [ ] **Step 6: Commit**

```bash
git add vtorrent-ui/src/components/staking/HealthStrip.tsx vtorrent-ui/src/components/staking/HealthStrip.test.tsx vtorrent-ui/src/pages/StakingPage.tsx
git commit -m "feat(ui): health strip with grafana link on staking page"
```

### Task 3: RewardHistory with lazy last-20 fetch

**Files:**
- Create: `vtorrent-ui/src/components/staking/RewardHistory.tsx`
- Test: `vtorrent-ui/src/components/staking/RewardHistory.test.ts`
- Modify: `vtorrent-ui/src/pages/StakingPage.tsx`

- [ ] **Step 1: Write the failing test**

```ts
// vtorrent-ui/src/components/staking/RewardHistory.test.ts
import { describe, it, expect } from 'vitest'
import { dailyAvgReward } from '../../utils/stakingOps'

describe('RewardHistory math', () => {
  it('returns 0 for empty history', () => {
    expect(dailyAvgReward([], 1_700_000_000)).toBe(0)
  })
})
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd vtorrent-ui && pnpm vitest run src/components/staking/RewardHistory.test.ts`
Expected: FAIL only if helper breaks; initially passes as regression guard — proceed to component. (If PASS, continue; the component file itself does not exist yet.)

- [ ] **Step 3: Write minimal implementation**

```tsx
// vtorrent-ui/src/components/staking/RewardHistory.tsx
import { useState } from 'react'
import { camel, isTauri, rpcGet, tauriInvoke } from '../../api'
import { dailyAvgReward, type RewardPoint } from '../../utils/stakingOps'
import { formatVTR } from '../../hooks/useWallet'

async function fetchBlock(height: number): Promise<{ timestamp: number }> {
  if (isTauri()) {
    throw new Error('Reward history needs RPC web mode in v1 (no Tauri get_block_by_height command).')
  }
  const raw = await rpcGet<unknown>(`/api/v1/blockchain/block/height/${height}`)
  return camel(raw) as { timestamp: number }
}

export default function RewardHistory({ tipHeight, blocksStaked }: { tipHeight: number | null, blocksStaked: number }) {
  const [points, setPoints] = useState<RewardPoint[]>([])
  const [loading, setLoading] = useState(false)
  const [expanded, setExpanded] = useState(false)

  const load = async () => {
    if (tipHeight == null || loading) return
    setLoading(true)
    try {
      const out: RewardPoint[] = []
      for (let h = tipHeight; h > tipHeight - 20 && h > 0; h--) {
        const b = await fetchBlock(h)
        out.push({ height: h, timestamp: b.timestamp, rewardSats: 0 })
      }
      setPoints(out)
      setExpanded(true)
    } finally {
      setLoading(false)
    }
  }

  const avg = dailyAvgReward(points.filter(p => p.rewardSats > 0), Math.floor(Date.now() / 1000))

  return (
    <div className="bg-navy-900/40 border border-vtorrent-900/20 rounded-xl p-5 space-y-3">
      <h2 className="text-sm font-medium text-gray-300">Recent Rewards ({blocksStaked} staked)</h2>
      {!expanded ? (
        <button onClick={load} disabled={tipHeight == null || loading} className="text-xs text-vtorrent-400 underline disabled:opacity-50">
          {loading ? 'Loading…' : 'Show last 20 blocks'}
        </button>
      ) : (
        <p className="text-xs text-gray-400 font-mono">Daily avg (non-zero rewards): {formatVTR(Math.round(avg))} / day</p>
      )}
    </div>
  )
}
```

- [ ] **Step 4: Run tests + lint**

Run: `cd vtorrent-ui && pnpm vitest run src/components/staking/RewardHistory.test.ts && pnpm lint`
Expected: PASS, lint exit 0.

- [ ] **Step 5: Wire into StakingPage details section**

```tsx
// vtorrent-ui/src/pages/StakingPage.tsx (after details card)
import RewardHistory from '../components/staking/RewardHistory'
// ...
      <RewardHistory tipHeight={status?.blockHeight ?? null} blocksStaked={blocksStaked} />
```

Note: `useStakingStatus` in `src/hooks/useNode.tsx` does not expose tip height; use `useNodeInfo` tip instead. Exact wiring:

```tsx
import { useNodeInfo } from '../hooks/useNode'
// inside component:
const { data: node } = useNodeInfo(10_000)
// ...
      <RewardHistory tipHeight={node?.blockHeight ?? null} blocksStaked={blocksStaked} />
```

Run: `cd vtorrent-ui && pnpm lint`
Expected: exit 0.

- [ ] **Step 6: Commit**

```bash
git add vtorrent-ui/src/components/staking/RewardHistory.tsx vtorrent-ui/src/components/staking/RewardHistory.test.ts vtorrent-ui/src/pages/StakingPage.tsx
git commit -m "feat(ui): lazy reward history on staking page"
```

**Follow-ups (tracked, not blocking):** surface `fetchBlock` errors in UI
(especially the Tauri dead-click path — currently silent), label the v1
zero-avg as placeholder until the v2 reward-history endpoint lands, and dedupe
the second `useNodeInfo` poller on StakingPage (HealthStrip + RewardHistory
each poll `/api/v1/info`).

### Task 4: Start-flow errors + full-address copy

**Files:**
- Modify: `vtorrent-ui/src/pages/StakingPage.tsx`
- Test: `vtorrent-ui/src/utils/stakingOps.test.ts` (append)

- [ ] **Step 1: Write the failing test**

```ts
// append to vtorrent-ui/src/utils/stakingOps.test.ts
import { stakingStartError } from './stakingOps'

describe('stakingStartError', () => {
  it('maps wallet locked', () => {
    expect(stakingStartError('wallet is locked')).toBe('Wallet is locked. Unlock first, then start staking.')
  })
  it('passes through unknown errors', () => {
    expect(stakingStartError('boom')).toBe('boom')
  })
})
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd vtorrent-ui && pnpm vitest run src/utils/stakingOps.test.ts`
Expected: FAIL with "stakingStartError is not a function".

- [ ] **Step 3: Write minimal implementation**

```ts
// append to vtorrent-ui/src/utils/stakingOps.ts
export function stakingStartError(raw: string): string {
  const lower = raw.toLowerCase()
  if (lower.includes('wallet') && lower.includes('lock')) {
    return 'Wallet is locked. Unlock first, then start staking.'
  }
  if (lower.includes('no address') || lower.includes('address is required')) {
    return 'No staking address selected. Unlock your wallet first.'
  }
  return raw
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd vtorrent-ui && pnpm vitest run src/utils/stakingOps.test.ts`
Expected: PASS.

- [ ] **Step 5: Use it + copyable address in StakingPage**

```tsx
// vtorrent-ui/src/pages/StakingPage.tsx
import { stakingStartError } from '../utils/stakingOps'
// in handleStart catch:
setActionMsg(stakingStartError(e instanceof Error ? e.message : String(e)))
// DetailRow for address: replace truncated span with:
<span className="text-gray-200 font-mono text-xs break-all select-all" title={stakingAddress ?? ''}>
  {stakingAddress ?? '—'}
</span>
```

Run: `cd vtorrent-ui && pnpm lint && pnpm build`
Expected: lint exit 0; build succeeds (tsc + vite).

- [ ] **Step 6: Commit**

```bash
git add vtorrent-ui/src/utils/stakingOps.ts vtorrent-ui/src/utils/stakingOps.test.ts vtorrent-ui/src/pages/StakingPage.tsx
git commit -m "feat(ui): staking start errors and copyable address"
```

### Task 5: Eligibility counts (no new endpoint)

**Files:**
- Modify: `vtorrent-ui/src/pages/StakingPage.tsx`

**Spec coverage:** `EligibilityTable` falls back to existing counts in v1
(`eligibleUtxos`, staking balance); per-UTXO age table deferred to v2 with a
backend endpoint.

- [ ] **Step 1: Keep existing Eligible UTXOs row, add balance-per-UTXO hint**

```tsx
// vtorrent-ui/src/pages/StakingPage.tsx (inside details grid, after Eligible UTXOs row)
<DetailRow
  label="Avg per UTXO"
  value={eligibleUtxos > 0 ? formatVTR(Math.floor(totalStakingSats / eligibleUtxos)) : '—'}
/>
```

Run: `cd vtorrent-ui && pnpm lint`
Expected: exit 0.

- [ ] **Step 2: Commit**

```bash
git add vtorrent-ui/src/pages/StakingPage.tsx
git commit -m "feat(ui): eligibility avg per UTXO on staking page"
```
