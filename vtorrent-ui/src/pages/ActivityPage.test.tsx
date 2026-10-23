// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router-dom'
import { afterEach, describe, expect, it, vi } from 'vitest'
import ActivityPage from './ActivityPage'
import type { ActivityItem } from '../hooks/useNode'

const items: ActivityItem[] = [
  {
    txid: 'confirmed-tx',
    status: 'confirmed',
    txType: 'receive',
    amountSatoshis: 100_000_000,
    timestamp: 1_700_000_000,
    blockHeight: 10,
    confirmations: 3,
    feeSatoshis: 0,
  },
  {
    txid: 'pending-tx',
    status: 'pending',
    txType: 'send',
    amountSatoshis: 50_000_000,
    timestamp: 1_700_000_100,
    blockHeight: 0,
    confirmations: 0,
    feeSatoshis: 1_000,
  },
]

vi.mock('../hooks/useNode', () => ({
  useActivity: () => ({
    data: items,
    loading: false,
    error: null,
    refresh: vi.fn(),
  }),
}))

vi.mock('../hooks/useWallet', () => ({
  formatVTR: (satoshis: number) => `${satoshis / 100_000_000} VTR`,
}))

describe('activity page', () => {
  afterEach(cleanup)

  it('shows confirmed and pending activity and opens details', () => {
    render(
      <MemoryRouter initialEntries={['/activity']}>
        <ActivityPage />
      </MemoryRouter>
    )

    expect(screen.getByText('confirmed-tx')).toBeInTheDocument()
    expect(screen.getByText('pending-tx')).toBeInTheDocument()
    expect(screen.getAllByText('Confirmed').length).toBeGreaterThan(0)
    expect(screen.getAllByText('Pending').length).toBeGreaterThan(0)

    fireEvent.click(screen.getByText('pending-tx').closest('button')!)
    expect(screen.getByRole('heading', { name: 'Transaction details' })).toBeInTheDocument()
    expect(screen.getByText(/This transaction is in the mempool/)).toBeInTheDocument()
  })

  it('filters by search text', () => {
    render(
      <MemoryRouter initialEntries={['/activity']}>
        <ActivityPage />
      </MemoryRouter>
    )
    fireEvent.change(screen.getByPlaceholderText('Search by txid or type'), {
      target: { value: 'pending-tx' },
    })
    expect(screen.queryByText('confirmed-tx')).not.toBeInTheDocument()
    expect(screen.getByText('pending-tx')).toBeInTheDocument()
  })
})
