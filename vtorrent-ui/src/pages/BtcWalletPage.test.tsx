// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import BtcWalletPage from './BtcWalletPage'

const backend = vi.hoisted(() => ({
  rebroadcast: vi.fn<(txid: string, passphrase?: string, otp?: string | null) => Promise<string | null>>(),
}))

vi.mock('../hooks/useBtc', () => ({
  useBtcStatus: () => ({
    status: {
      initialized: true,
      balanceSatoshis: 0,
      address: null,
      bestHeight: 1,
      synced: true,
      balanceHidden: false,
    },
    error: null,
  }),
  useBtcAddress: () => ({ address: null, generate: vi.fn(), loading: false, error: null }),
  useSendBtc: () => ({
    send: vi.fn(),
    rebroadcast: backend.rebroadcast,
    loading: false,
    error: null,
  }),
  usePendingBtcSends: () => [
    { txid: 'ab'.repeat(32), hasRawTx: true, inputs: [{ txid: 'cd'.repeat(32), vout: 0, valueSatoshis: 1000 }] },
  ],
}))

afterEach(cleanup)

describe('BTC pending sends', () => {
  it('retries a retained send with the wallet passphrase', async () => {
    backend.rebroadcast.mockResolvedValue('ab'.repeat(32))
    render(<BtcWalletPage />)
    expect(screen.getByText('ab'.repeat(32))).toBeInTheDocument()
    fireEvent.change(screen.getByPlaceholderText('wallet passphrase'), {
      target: { value: 'test-passphrase' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }))
    await waitFor(() =>
      expect(backend.rebroadcast).toHaveBeenCalledWith('ab'.repeat(32), 'test-passphrase', null)
    )
  })
})
