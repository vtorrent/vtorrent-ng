// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import WelcomePage from './WelcomePage'

const navigate = vi.fn()

vi.mock('react-router-dom', async importOriginal => ({
  ...await importOriginal<typeof import('react-router-dom')>(),
  useNavigate: () => navigate,
}))

vi.mock('../hooks/useWallet', () => ({
  useWallet: () => ({ unlock: vi.fn() }),
}))

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
