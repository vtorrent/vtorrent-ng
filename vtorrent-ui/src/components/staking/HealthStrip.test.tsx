import { describe, it, expect } from 'vitest'
import { healthSummary } from '../../utils/stakingOps'

function stripState(data: { blockHeight: number, connections: number, syncing: boolean, syncPercent: number, mempoolSize: number } | null, loading: boolean, error: string | null): string {
  if (!data) {
    if (error) return `error:${error}`
    return 'loading'
  }
  return healthSummary(data) + (error ? ' (refresh failed)' : '')
}

describe('HealthStrip states', () => {
  it('shows loading when no data and no error', () => {
    expect(stripState(null, true, null)).toBe('loading')
  })

  it('shows error when no data and fetch failed', () => {
    expect(stripState(null, false, 'down')).toBe('error:down')
  })

  it('keeps stale data with refresh note on warm failure', () => {
    const text = stripState({ blockHeight: 3660, connections: 2, syncing: false, syncPercent: 100, mempoolSize: 0 }, false, 'timeout')
    expect(text).toContain('3660')
    expect(text).toContain('(refresh failed)')
  })
})
