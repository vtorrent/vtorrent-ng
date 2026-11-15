// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import BtcWalletPage from './BtcWalletPage'

const backend = vi.hoisted(() => ({
  send: vi.fn<() => Promise<string | null>>(),
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
    send: backend.send,
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
  it('does not claim that submission proves acceptance or confirmation', async () => {
    backend.send.mockResolvedValue('ef'.repeat(32))
    render(<BtcWalletPage />)
    fireEvent.click(screen.getByRole('button', { name: 'Send' }))
    expect(await screen.findByText(/submitted.*unconfirmed/i)).toBeInTheDocument()
    expect(screen.queryByText(/Sent!/)).not.toBeInTheDocument()
  })

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
