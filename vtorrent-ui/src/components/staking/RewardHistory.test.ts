import { describe, it, expect } from 'vitest'
import { dailyAvgReward } from '../../utils/stakingOps'

describe('RewardHistory math', () => {
  it('returns 0 for empty history', () => {
    expect(dailyAvgReward([], 1_700_000_000)).toBe(0)
  })
  it('averages non-zero rewards per day', () => {
    const now = 1_700_000_000
    const points = [
      { height: 100, timestamp: now - 3600, rewardSats: 100_000 },
      { height: 101, timestamp: now - 1800, rewardSats: 300_000 },
    ]
    expect(dailyAvgReward(points, now)).toBeCloseTo(400_000 * (86400 / 3600), 0)
  })
})
