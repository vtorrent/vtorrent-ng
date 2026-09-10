import { describe, it, expect } from 'vitest'
import { dailyAvgReward } from '../../utils/stakingOps'

describe('RewardHistory math', () => {
  it('returns 0 for empty history', () => {
    expect(dailyAvgReward([], 1_700_000_000)).toBe(0)
  })
})
