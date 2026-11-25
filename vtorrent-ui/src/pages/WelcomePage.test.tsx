// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import WelcomePage from './WelcomePage'

const wallet = vi.hoisted(() => ({
  unlock: vi.fn(),
  startNode: vi.fn(),
  repairBtcState: vi.fn(),
}))
const navigate = vi.hoisted(() => vi.fn())

vi.mock('react-router-dom', async importOriginal => ({
  ...await importOriginal<typeof import('react-router-dom')>(),
  useNavigate: () => navigate,
}))

vi.mock('../hooks/useWallet', () => ({ useWallet: () => wallet }))

vi.mock('../hooks/useNetwork', () => ({
  useNetwork: () => ({
    network: 'mainnet',
    setNetwork: vi.fn(),
    seeds: '',
    setSeeds: vi.fn(),
  }),
  parseSeeds: () => [],
}))

vi.mock('../hooks/useNode', () => ({
  useNodeInfo: () => ({ data: null, loading: false }),
}))

vi.mock('../components/AppIcon', () => ({
  default: () => <div data-testid="app-icon" />,
}))

vi.mock('../components/NetworkPicker', () => ({
  default: () => <div data-testid="network-picker" />,
}))

function nodeStartFailure(type: string) {
  const error = new Error('Wallet unlocked, but the node failed to start')
  ;(error as Error & { cause?: unknown }).cause = { type, message: `${type} detail` }
  return error
}

async function showRepairFor(type: string) {
  wallet.unlock.mockRejectedValueOnce(nodeStartFailure(type))
  render(<WelcomePage />)
  fireEvent.click(screen.getByText('Open Wallet'))
  fireEvent.change(screen.getByPlaceholderText('Enter your wallet passphrase'), {
    target: { value: 'unlock-pass' },
  })
  fireEvent.click(screen.getByRole('button', { name: 'Unlock' }))
  await screen.findByRole('heading', { name: 'Repair BTC state' })
}

describe('welcome onboarding', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })
  afterEach(cleanup)

  it('guides a staking goal to wallet creation', () => {
    render(<WelcomePage />)
    const stake = screen.getByRole('button', { name: 'Stake VTR' })
    fireEvent.click(stake)
    expect(stake).toHaveAttribute('aria-pressed', 'true')
    expect(screen.getByText(/then start staking/)).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'Create wallet' }))
    expect(navigate).toHaveBeenCalledWith('/create')
  })

  it('offers recovery paths for legacy holders', () => {
    render(<WelcomePage />)
    fireEvent.click(screen.getByRole('button', { name: 'Recover old VTR' }))
    expect(screen.getByRole('button', { name: 'Import legacy wallet' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Restore backup' })).toBeInTheDocument()
  })
})

describe('welcome unlock recovery', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    wallet.startNode.mockResolvedValue(undefined)
    wallet.repairBtcState.mockResolvedValue('BTC state is ready; restart the node')
  })
  afterEach(cleanup)

  it('offers authenticated BTC repair when the node refuses a pending repair', async () => {
    wallet.unlock.mockRejectedValueOnce(nodeStartFailure('BtcRepairPending'))
    render(<WelcomePage />)

    fireEvent.click(screen.getByText('Open Wallet'))
    fireEvent.change(screen.getByPlaceholderText('Enter your wallet passphrase'), {
      target: { value: 'unlock-pass' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Unlock' }))

    expect(
      await screen.findByRole('heading', { name: 'Repair BTC state' })
    ).toBeInTheDocument()
    expect(wallet.unlock).toHaveBeenCalledWith('unlock-pass', undefined, {
      network: 'mainnet',
      seeds: [],
    })

    fireEvent.change(screen.getByPlaceholderText('Wallet passphrase'), {
      target: { value: 'repair-pass' },
    })
    fireEvent.change(screen.getByPlaceholderText('2FA code (if enabled)'), {
      target: { value: '123456' },
    })
    const repairButton = screen.getByRole('button', { name: 'Repair BTC state' })
    expect(repairButton).toBeDisabled()
    fireEvent.click(repairButton)
    expect(wallet.repairBtcState).not.toHaveBeenCalled()
    expect(
      screen.queryByText(/Confirm that this wallet has no prior BTC history first/)
    ).not.toBeInTheDocument()

    fireEvent.click(screen.getByRole('checkbox'))
    fireEvent.click(repairButton)

    await waitFor(() =>
      expect(wallet.repairBtcState).toHaveBeenCalledWith('repair-pass', '123456', true)
    )
    await waitFor(() =>
      expect(wallet.startNode).toHaveBeenCalledWith({
        network: 'mainnet',
        seeds: [],
      })
    )
    await waitFor(() => expect(navigate).toHaveBeenCalledWith('/dashboard'))
  })

  it('does not offer repair for a generic passphrase failure', async () => {
    wallet.unlock.mockRejectedValueOnce({
      type: 'Unauthorized',
      message: 'Incorrect wallet passphrase',
    })
    render(<WelcomePage />)

    fireEvent.click(screen.getByText('Open Wallet'))
    fireEvent.change(screen.getByPlaceholderText('Enter your wallet passphrase'), {
      target: { value: 'wrong-pass' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Unlock' }))

    expect(await screen.findByText('Incorrect wallet passphrase')).toBeInTheDocument()
    expect(
      screen.queryByRole('heading', { name: 'Repair BTC state' })
    ).not.toBeInTheDocument()
  })

  it('describes a missing BTC state and offers repair', async () => {
    await showRepairFor('BtcStateMissing')
    expect(
      screen.getByText(/BTC state is missing\. If this wallet has no prior BTC history/)
    ).toBeInTheDocument()
  })

  it('describes an assisted-recovery conflict and preserves the files advice', async () => {
    await showRepairFor('BtcAssistedRecovery')
    expect(
      screen.getByText(/conflicts with an existing recovery record/)
    ).toBeInTheDocument()
    expect(screen.getByText(/authoritative conflict result/)).toBeInTheDocument()
  })
})
