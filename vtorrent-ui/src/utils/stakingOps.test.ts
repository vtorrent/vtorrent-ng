import { describe, it, expect } from 'vitest'
import { dailyAvgReward, maturityCountdown, healthSummary, stakingStartError } from './stakingOps'

describe('stakingOps', () => {
  it('averages recent rewards per day', () => {
    const now = 1_700_000_000
    const rewards = [
      { height: 100, timestamp: now - 3600, rewardSats: 100_000 },
      { height: 101, timestamp: now - 1800, rewardSats: 300_000 },
    ]
    expect(dailyAvgReward(rewards, now)).toBeCloseTo(400_000 * (86400 / 3600), 0)
  })

  it('returns 0 for empty history', () => {
    expect(dailyAvgReward([], 1_700_000_000)).toBe(0)
  })

  it('counts down maturity in blocks', () => {
    expect(maturityCountdown(90, 100)).toBe(10)
    expect(maturityCountdown(100, 100)).toBe(0)
  })

  it('clamps over-mature countdown to zero', () => {
    expect(maturityCountdown(150, 100)).toBe(0)
  })

  it('summarizes health', () => {
    const s = healthSummary({ blockHeight: 10, connections: 2, syncing: false, syncPercent: 100, mempoolSize: 0 })
    expect(s).toBe('height 10 · 2 peers · synced · mempool 0')
  })

  it('reports syncing branch', () => {
    const s = healthSummary({ blockHeight: 5, connections: 1, syncing: true, syncPercent: 99.9, mempoolSize: 3 })
    expect(s).toContain('syncing')
  })

  it('maps wallet locked', () => {
    expect(stakingStartError('wallet is locked')).toBe('Wallet is locked. Unlock first, then start staking.')
  })

  it('passes through unknown errors', () => {
    expect(stakingStartError('boom')).toBe('boom')
  })
})
