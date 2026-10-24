// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { cleanup, render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { afterEach, describe, expect, it, vi } from 'vitest'
import Layout from './Layout'

vi.mock('../hooks/useWallet', () => ({
  useWallet: () => ({
    lock: vi.fn(),
    totalBalance: 0,
    has2FA: false,
    keys: [],
  }),
  formatVTR: (satoshis: number) => `${satoshis} VTR`,
}))

vi.mock('../hooks/useNode', () => ({
  useNodeInfo: () => ({ data: null, loading: false }),
}))

vi.mock('../hooks/useNotifications', () => ({
  useNotifications: () => ({ unread: 0 }),
}))

vi.mock('../hooks/useTheme', () => ({
  useTheme: () => ({ theme: 'modern', toggle: vi.fn() }),
}))

vi.mock('./AppIcon', () => ({
  default: () => <div data-testid="app-icon" />,
}))

describe('layout navigation', () => {
  afterEach(cleanup)

  it('groups destinations and labels icon-only controls', () => {
    render(
      <MemoryRouter initialEntries={['/']}>
        <Routes>
          <Route element={<Layout />}>
            <Route path="/" element={<div>page</div>} />
          </Route>
        </Routes>
      </MemoryRouter>
    )

    expect(screen.getByText('Wallet')).toBeInTheDocument()
    expect(screen.getByText('Earn')).toBeInTheDocument()
    expect(screen.getByText('Trade')).toBeInTheDocument()
    expect(screen.getByText('System')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'Dashboard' })).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'Staking' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Lock Wallet' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Notifications' })).toBeInTheDocument()
  })
})
