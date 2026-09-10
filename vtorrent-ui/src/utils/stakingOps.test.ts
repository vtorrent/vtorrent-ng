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
