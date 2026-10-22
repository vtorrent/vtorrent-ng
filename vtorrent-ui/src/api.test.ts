import { describe, it, expect } from 'vitest'
import { camel, formatInvokeError } from './api'

describe('camel', () => {
  it('normalizes Tauri snake_case node info to camelCase', () => {
    const raw = {
      version: '2.0.0-beta.3',
      network: 'vtorrent-regtest',
      block_height: 42,
      best_block_hash: 'ab',
      connections: 3,
      syncing: false,
      sync_percent: 100.0,
      mempool_size: 0,
      uptime_secs: 99,
    }
    const info = camel(raw) as { blockHeight: number, syncPercent: number, mempoolSize: number, uptimeSecs: number }
    expect(info.blockHeight).toBe(42)
    expect(info.blockHeight.toLocaleString()).toBe('42')
    expect(info.syncPercent).toBe(100.0)
    expect(info.mempoolSize).toBe(0)
    expect(info.uptimeSecs).toBe(99)
  })
})


describe('formatInvokeError', () => {
  it('uses the Tauri error message', () => {
    expect(formatInvokeError({ type: 'InvalidInput', message: 'A wallet already exists' }, 'fallback'))
      .toBe('A wallet already exists')
  })

  it('falls back for unknown values', () => {
    expect(formatInvokeError(null, 'fallback')).toBe('fallback')
  })
})
