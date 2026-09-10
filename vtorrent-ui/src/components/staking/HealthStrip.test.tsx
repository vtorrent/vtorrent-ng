import { describe, it, expect } from 'vitest'
import { healthSummary } from '../../utils/stakingOps'

describe('HealthStrip data', () => {
  it('renders synced summary', () => {
    const text = healthSummary({ blockHeight: 3660, connections: 2, syncing: false, syncPercent: 100, mempoolSize: 0 })
    expect(text).toContain('3660')
    expect(text).toContain('synced')
  })
})
