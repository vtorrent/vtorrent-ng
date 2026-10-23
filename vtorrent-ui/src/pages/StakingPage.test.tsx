// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { ColdStakeKeys } from '../hooks/useNode'
import StakingPage from './StakingPage'

const backend = vi.hoisted(() => ({
  create: vi.fn<(locktime?: number) => Promise<ColdStakeKeys>>(),
  fund: vi.fn<(script: string, amount: number, passphrase?: string) => Promise<string>>(),
  desktop: vi.fn<() => boolean>(),
}))

vi.mock('../hooks/useNode', () => ({
  useStakingStatus: () => ({ data: null, loading: false, error: null, refresh: vi.fn() }),
  useNodeInfo: () => ({ data: null, loading: false, error: null }),
  startStaking: vi.fn(),
  stopStaking: vi.fn(),
  createColdStake: backend.create,
  fundColdStake: backend.fund,
  getTransaction: vi.fn(),
}))

vi.mock('../hooks/useWallet', () => ({
  useWallet: () => ({ keys: [], totalBalance: 0 }),
  formatVTR: (satoshis: number) => `${satoshis / 100_000_000} VTR`,
}))

vi.mock('../api', async importOriginal => ({
  ...await importOriginal<typeof import('../api')>(),
  isTauri: backend.desktop,
}))

const keys: ColdStakeKeys = {
  scriptPubkey: '63' + 'ab'.repeat(59),
  scriptHash: 'cd'.repeat(20),
  stakingWif: 'test-hot-key',
  spendingWif: 'test-cold-key',
  stakingAddress: 'VStakingAddress',
  spendingAddress: 'VSpendingAddress',
  locktime: 0,
}

async function generate() {
  fireEvent.click(screen.getByRole('button', { name: 'Generate key pair' }))
  await screen.findByText(keys.scriptPubkey)
}

function saveSpendingKey() {
  fireEvent.click(screen.getByTestId('cold-stake-saved-spending'))
}

describe('cold-stake panel', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    backend.desktop.mockReturnValue(false)
    backend.create.mockResolvedValue(keys)
    backend.fund.mockResolvedValue('test-funding-txid')
  })
  afterEach(cleanup)

  it('generates keys and funds the displayed raw script in browser mode', async () => {
    render(<StakingPage />)
    await generate()
    saveSpendingKey()
    expect(backend.create).toHaveBeenCalledWith(0)
    expect(screen.getByText(keys.scriptHash)).toBeInTheDocument()
    expect(screen.queryByText('P2CS address (fund this)')).not.toBeInTheDocument()

    fireEvent.change(screen.getByPlaceholderText('Amount (VTR)'), { target: { value: '1.25' } })
    fireEvent.change(screen.getByPlaceholderText('Wallet passphrase'), { target: { value: 'test-passphrase' } })
    fireEvent.click(screen.getByRole('button', { name: 'Fund cold stake' }))

    await waitFor(() => expect(backend.fund).toHaveBeenCalledWith(keys.scriptPubkey, 125_000_000, 'test-passphrase'))
    expect(await screen.findByText('Funding txid: test-funding-txid')).toBeInTheDocument()
  })

  it('funds in desktop mode without collecting a browser passphrase', async () => {
    backend.desktop.mockReturnValue(true)
    render(<StakingPage />)
    await generate()
    saveSpendingKey()
    expect(screen.queryByPlaceholderText('Wallet passphrase')).not.toBeInTheDocument()
    fireEvent.change(screen.getByPlaceholderText('Amount (VTR)'), { target: { value: '1' } })
    fireEvent.click(screen.getByRole('button', { name: 'Fund cold stake' }))
    await waitFor(() => expect(backend.fund).toHaveBeenCalledWith(keys.scriptPubkey, 100_000_000, ''))
    expect(await screen.findByText('Funding txid: test-funding-txid')).toBeInTheDocument()
  })

  it('rejects a non-positive amount before calling the backend', async () => {
    render(<StakingPage />)
    await generate()
    saveSpendingKey()
    fireEvent.change(screen.getByPlaceholderText('Amount (VTR)'), { target: { value: '0' } })
    fireEvent.click(screen.getByRole('button', { name: 'Fund cold stake' }))
    expect(await screen.findByText('Enter a positive VTR amount')).toBeInTheDocument()
    expect(backend.fund).not.toHaveBeenCalled()
  })

  it('shows a funding failure and allows retrying', async () => {
    backend.fund.mockRejectedValueOnce(new Error('Wallet locked'))
    render(<StakingPage />)
    await generate()
    saveSpendingKey()
    fireEvent.change(screen.getByPlaceholderText('Amount (VTR)'), { target: { value: '1' } })
    fireEvent.click(screen.getByRole('button', { name: 'Fund cold stake' }))
    expect(await screen.findByText('Wallet locked')).toBeInTheDocument()
    expect(screen.queryByText(/Funding txid:/)).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Fund cold stake' })).toBeEnabled()
    fireEvent.click(screen.getByRole('button', { name: 'Fund cold stake' }))
    expect(await screen.findByText('Funding txid: test-funding-txid')).toBeInTheDocument()
  })
})
