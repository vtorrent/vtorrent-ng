// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import SettingsPage from './SettingsPage'
import type { AppInfo, NodeInfo } from '../hooks/useNode'

const backend = vi.hoisted(() => ({
  setPeer: vi.fn(),
  diagnostics: vi.fn(),
  restart: vi.fn(),
}))

const node: NodeInfo = {
  version: '2.0.0-beta.3',
  network: 'vtorrent-regtest',
  blockHeight: 42,
  bestBlockHash: '00',
  connections: 2,
  syncing: false,
  syncPercent: 100,
  mempoolSize: 1,
  uptimeSecs: 3600,
}

const appInfo: AppInfo = {
  version: '2.0.0-beta.3',
  network: 'vtorrent-regtest',
  appDataDir: '/home/user/.local/share/io.vtorrent.client',
  nodeDataDir: '/home/user/.vtorrent/testnet',
  btcNetwork: 'Regtest',
  btcPeer: '127.0.0.1:18448',
  walletAddress: 'VTestWalletAddress',
}

vi.mock('../hooks/useNode', () => ({
  useNodeInfo: () => ({ data: node, loading: false, error: null }),
  useAppInfo: () => ({
    data: appInfo,
    loading: false,
    error: null,
    refresh: vi.fn(),
  }),
  getDiagnostics: backend.diagnostics,
  setBtcPeer: backend.setPeer,
  restartApp: backend.restart,
}))

vi.mock('../api', async importOriginal => ({
  ...await importOriginal<typeof import('../api')>(),
  isTauri: () => true,
}))

describe('settings page', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    backend.setPeer.mockResolvedValue(undefined)
    backend.diagnostics.mockResolvedValue('{"network":"vtorrent-regtest"}')
    backend.restart.mockResolvedValue(undefined)
  })
  afterEach(cleanup)

  it('shows node paths and saves the BTC peer', async () => {
    render(<SettingsPage />)
    expect(screen.getByText('/home/user/.local/share/io.vtorrent.client')).toBeInTheDocument()
    expect(screen.getByText('/home/user/.vtorrent/testnet')).toBeInTheDocument()
    expect(screen.getByDisplayValue('127.0.0.1:18448')).toBeInTheDocument()

    fireEvent.change(screen.getByPlaceholderText('127.0.0.1:18448'), {
      target: { value: '127.0.0.1:18449' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(backend.setPeer).toHaveBeenCalledWith('127.0.0.1:18449'))
  })

  it('previews redacted diagnostics', async () => {
    render(<SettingsPage />)
    fireEvent.click(screen.getByRole('button', { name: 'Preview diagnostics' }))
    expect(await screen.findByText('{"network":"vtorrent-regtest"}')).toBeInTheDocument()
    expect(backend.diagnostics).toHaveBeenCalled()
  })
})
