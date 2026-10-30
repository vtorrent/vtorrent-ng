// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import SwapWorkspace from './SwapWorkspace'
import type { DexOrder, SwapChainStatus } from '../hooks/useNode'

const backend = vi.hoisted(() => ({
  match: vi.fn(),
  fundBtc: vi.fn(),
  claimVtr: vi.fn(),
  claimBtc: vi.fn(),
  refund: vi.fn(),
  reconcile: vi.fn(),
  status: vi.fn(),
  deadlines: vi.fn(),
}))

vi.mock('../hooks/useWallet', () => ({
  useWallet: () => ({ defaultAddress: 'VMe' }),
  formatVTR: (satoshis: number) => `${satoshis / 100_000_000} VTR`,
}))

vi.mock('../hooks/useBtc', () => ({
  useBtcAddress: () => ({
    address: 'bcrt1qtest',
    generate: vi.fn(),
    loading: false,
    error: null,
  }),
}))

vi.mock('../hooks/useNode', () => ({
  matchDexOrder: backend.match,
  btcFund: backend.fundBtc,
  vtrClaim: backend.claimVtr,
  btcClaim: backend.claimBtc,
  swapRefund: backend.refund,
  reconcileBtcSwap: backend.reconcile,
  getSwapStatus: backend.status,
  getSwapDeadlines: backend.deadlines,
}))

const order: DexOrder = {
  id: 'order-1',
  makerAddress: 'VOther',
  offerAmountSatoshis: 100_000_000,
  offerAsset: 'VTR',
  requestAmountSatoshis: 1_000_000,
  requestAsset: 'BTC',
  rate: 0.01,
  status: 'matched',
  createdAt: 1,
  expiresAt: 9_999_999_999,
}

const fundedVtr: SwapChainStatus = {
  orderId: 'order-1',
  btcReconciled: false,
  btc: null,
  vtr: {
    tipHash: '',
    tipHeight: 1,
    state: 'funded',
    funding: { txid: 'vtr-fund', blockHash: '', height: 1, confirmations: 1 },
    spend: null,
    pendingSpendTxid: null,
    reorgCount: 0,
  },
}

const fundedBoth: SwapChainStatus = {
  ...fundedVtr,
  btc: {
    network: 'regtest',
    observedAt: 1,
    tipHash: '',
    tipHeight: 1,
    scanStart: 0,
    state: 'funded',
    funding: { txid: 'btc-fund', blockHash: '', height: 1, confirmations: 1 },
    spend: null,
    reorgCount: 0,
  },
}

describe('swap workspace', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    backend.status.mockResolvedValue(fundedVtr)
    backend.deadlines.mockResolvedValue([{
      orderId: 'order-1',
      status: 'BtcFunding',
      btcExpiry: 9_999_999_999,
      secondsRemaining: 3600,
      atRisk: true,
    }])
    backend.match.mockResolvedValue({})
    backend.fundBtc.mockResolvedValue({})
    backend.claimVtr.mockResolvedValue({})
  })
  afterEach(cleanup)

  it('matches an open order from the guided workspace', async () => {
    render(<SwapWorkspace orders={[{ ...order, status: 'open' }]} onRefresh={vi.fn()} />)
    expect(await screen.findByText('Match this order')).toBeInTheDocument()
    fireEvent.change(screen.getByPlaceholderText('Wallet passphrase'), {
      target: { value: 'password' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Match order' }))
    await waitFor(() => expect(backend.match).toHaveBeenCalledWith({
      orderId: 'order-1',
      takerAddress: 'VMe',
      passphrase: 'password',
    }))
  })

  it('guides the taker to fund BTC with their refund address', async () => {
    render(<SwapWorkspace orders={[order]} onRefresh={vi.fn()} />)
    expect(await screen.findByText('Fund the BTC side')).toBeInTheDocument()
    expect(screen.getByDisplayValue('bcrt1qtest')).toBeInTheDocument()
    fireEvent.change(screen.getByPlaceholderText('Wallet passphrase'), { target: { value: 'test-pass' } })
    fireEvent.click(screen.getByRole('button', { name: 'Fund BTC' }))
    await waitFor(() => expect(backend.fundBtc).toHaveBeenCalledWith({
      orderId: 'order-1',
      btcRefundAddress: 'bcrt1qtest',
      passphrase: 'test-pass',
    }))
  })

  it('guides the taker to claim VTR after both sides are funded', async () => {
    backend.status.mockResolvedValue(fundedBoth)
    render(<SwapWorkspace orders={[order]} onRefresh={vi.fn()} />)
    expect(await screen.findByRole('button', { name: 'Claim VTR' })).toBeInTheDocument()
    fireEvent.change(screen.getByPlaceholderText('Swap secret (preimage, 64 hex chars)'), {
      target: { value: 'ab'.repeat(32) },
    })
    fireEvent.change(screen.getByPlaceholderText('Wallet passphrase'), {
      target: { value: 'password' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Claim VTR' }))
    await waitFor(() => expect(backend.claimVtr).toHaveBeenCalledWith({
      orderId: 'order-1',
      preimage: 'ab'.repeat(32),
      passphrase: 'password',
    }))
  })
})
