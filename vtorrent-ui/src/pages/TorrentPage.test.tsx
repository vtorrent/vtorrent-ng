// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import TorrentPage from './TorrentPage'

vi.mock('../hooks/useNode', () => ({
  useTorrentSessions: () => ({
    data: [{
      id: 'torrent-1',
      name: 'Test torrent',
      infoHash: 'abc123',
      state: 'Downloading',
      progress: 0.25,
      sizeBytes: 1024,
      downloadedBytes: 256,
      uploadedBytes: 0,
      downloadSpeed: 0,
      uploadSpeed: 0,
      peerCount: 1,
      vtrEarnedSatoshis: 0,
      vtrPaidSatoshis: 0,
      vtrPendingEarnedSatoshis: 0,
      vtrPendingOwedSatoshis: 0,
      vtrSettledEarnedSatoshis: 0,
      vtrSettledPaidSatoshis: 0,
    }],
    loading: false,
    error: null,
    refresh: vi.fn(),
  }),
  addTorrent: vi.fn(),
  removeTorrent: vi.fn(),
  useTorrentSettings: () => ({
    data: {
      downloadDir: '/tmp/downloads',
      downloadLimitBytesPerSec: 0,
      uploadLimitBytesPerSec: 0,
      seedRatioTarget: 0,
    },
    loading: false,
    error: null,
    refresh: vi.fn(),
  }),
  getTorrentSettings: vi.fn(),
  setTorrentSettings: vi.fn(),
}))

vi.mock('../hooks/useWallet', () => ({
  formatVTR: (satoshis: number) => `${satoshis / 100_000_000} VTR`,
}))

describe('torrent controls', () => {
  afterEach(cleanup)

  it('marks pause and resume as unavailable until the backend supports it', () => {
    render(<TorrentPage />)
    const pause = screen.getByRole('button', { name: /Pause torrent/ })
    expect(pause).toBeDisabled()
    expect(pause).toHaveAttribute('title', 'Pause and resume are not available yet')
  })
})
