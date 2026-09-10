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

import { describe as describe2, it as it2, expect as expect2 } from 'vitest'
import { dailyAvgReward as avg2, healthSummary as healthSummary2, maturityCountdown as maturityCountdown2 } from './stakingOps'

describe2('stakingOps carryover', () => {
  it2('returns 0 for empty history', () => {
    expect2(avg2([], 1_700_000_000)).toBe(0)
  })

  it2('reports syncing branch', () => {
    const s = healthSummary2({ blockHeight: 5, connections: 1, syncing: true, syncPercent: 99.9, mempoolSize: 3 })
    expect2(s).toContain('syncing')
  })

  it2('clamps over-mature countdown to zero', () => {
    expect2(maturityCountdown2(150, 100)).toBe(0)
  })
})
