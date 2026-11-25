// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { act, renderHook } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { btcRecoveryKind, formatInvokeError } from '../api'
import { WalletProvider, useWallet } from './useWallet'

const backend = vi.hoisted(() => ({ invoke: vi.fn() }))

vi.mock('../api', async importOriginal => ({
  ...await importOriginal<typeof import('../api')>(),
  isTauri: () => true,
  tauriInvoke: backend.invoke,
}))

vi.mock('@tauri-apps/api/path', () => ({
  appDataDir: async () => '/tmp/vtr-wallet-test',
  join: async (...parts: string[]) => parts.join('/'),
}))

describe('useWallet node-start error handoff', () => {
  beforeEach(() => {
    backend.invoke.mockReset()
    backend.invoke.mockImplementation(async (cmd: string) => {
      switch (cmd) {
        case 'open_wallet':
          return {
            is_unlocked: true,
            has_2fa: false,
            address_count: 1,
            default_address: 'VTestAddress',
            wallet_version: 2,
          }
        case 'get_addresses':
          return []
        case 'start_node':
          throw { type: 'BtcRepairPending', message: 'pending detail' }
        default:
          throw new Error(`unexpected command ${cmd}`)
      }
    })
  })

  it('preserves the typed cause through the real unlock wrapper', async () => {
    const { result } = renderHook(() => useWallet(), { wrapper: WalletProvider })
    let caught: unknown
    await act(async () => {
      try {
        await result.current.unlock('test-pass')
      } catch (error) {
        caught = error
      }
    })

    expect(caught).toBeInstanceOf(Error)
    expect(btcRecoveryKind(caught)).toBe('pending_repair')
    expect(formatInvokeError(caught, 'fallback')).toContain('pending detail')
    expect((caught as Error & { cause?: unknown }).cause).toEqual({
      type: 'BtcRepairPending',
      message: 'pending detail',
    })
  })
})
