// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router-dom'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import DashboardPage from './DashboardPage'

const wallet = vi.hoisted(() => ({
  quote: vi.fn(),
  send: vi.fn(),
}))

vi.mock('../hooks/useWallet', () => ({
  useWallet: () => ({
    keys: [{
      address: 'VTestAddress1234567890',
      label: 'Primary',
      isLegacyImport: false,
      balance: 100_000_000,
      createdAt: 0,
    }],
    defaultAddress: 'VTestAddress1234567890',
    totalBalance: 100_000_000,
    legacyImportCount: 0,
    generateAddress: vi.fn(),
    quoteVtr: wallet.quote,
    sendVtr: wallet.send,
  }),
  formatVTR: (satoshis: number) => `${satoshis / 100_000_000} VTR`,
}))

vi.mock('../hooks/useNode', () => ({
  useNodeInfo: () => ({
    data: {
      blockHeight: 1,
      connections: 1,
      syncing: false,
      syncPercent: 100,
      mempoolSize: 0,
    },
    loading: false,
  }),
  useTransactions: () => ({
    data: [],
    loading: false,
    error: null,
    refresh: vi.fn(),
  }),
  createPaymentRequest: vi.fn(),
}))

vi.mock('qrcode.react', () => ({
  default: () => <div data-testid="qr-code" />,
}))

describe('send review', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    wallet.quote.mockResolvedValue({
      fromAddress: 'VTestAddress1234567890',
      toAddress: 'VRecipient1234567890',
      amountSatoshis: 150_000_000,
      feeSatoshis: 1_000,
      totalDebitSatoshis: 150_001_000,
      changeSatoshis: 49_999_000,
      inputCount: 1,
      feeRate: 10,
      network: 'vtorrent-mainnet',
    })
    wallet.send.mockResolvedValue('a'.repeat(64))
  })
  afterEach(cleanup)

  it('shows the backend quote before sending', async () => {
    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>
    )

    fireEvent.click(screen.getByRole('button', { name: 'Send VTR' }))
    fireEvent.change(screen.getByPlaceholderText('V…'), {
      target: { value: 'VRecipient1234567890' },
    })
    fireEvent.change(screen.getByPlaceholderText('0.00000000'), {
      target: { value: '1.5' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Review' }))

    await waitFor(() => expect(wallet.quote).toHaveBeenCalledWith(
      'VRecipient1234567890',
      150_000_000
    ))
    expect(await screen.findByText('Review transaction')).toBeInTheDocument()
    expect(screen.getByText('Network fee')).toBeInTheDocument()
    expect(screen.getByText('0.00001 VTR')).toBeInTheDocument()
    expect(screen.getByText('1.50001 VTR')).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'Confirm & Send' }))
    await waitFor(() => expect(wallet.send).toHaveBeenCalledWith(
      'VRecipient1234567890',
      150_000_000
    ))
    expect(await screen.findByText('Transaction Sent')).toBeInTheDocument()
    expect(screen.getByText('a'.repeat(64))).toBeInTheDocument()
  })
})
