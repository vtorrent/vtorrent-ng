// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import TorrentSettingsPanel from './TorrentSettingsPanel'

const backend = vi.hoisted(() => ({
  set: vi.fn(),
}))

vi.mock('../hooks/useNode', () => ({
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
  setTorrentSettings: backend.set,
}))

describe('torrent settings panel', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    backend.set.mockImplementation(async settings => settings)
  })
  afterEach(cleanup)

  it('saves the directory, limits, and seed ratio', async () => {
    render(<TorrentSettingsPanel />)
    fireEvent.change(screen.getByTestId('torrent-download-dir'), {
      target: { value: '/tmp/torrents' },
    })
    fireEvent.change(screen.getByTestId('torrent-download-limit'), {
      target: { value: '100' },
    })
    fireEvent.change(screen.getByTestId('torrent-upload-limit'), {
      target: { value: '50' },
    })
    fireEvent.change(screen.getByTestId('torrent-seed-ratio'), {
      target: { value: '1.5' },
    })
    fireEvent.click(screen.getByTestId('torrent-settings-save'))

    await waitFor(() => expect(backend.set).toHaveBeenCalledWith({
      downloadDir: '/tmp/torrents',
      downloadLimitBytesPerSec: 102400,
      uploadLimitBytesPerSec: 51200,
      seedRatioTarget: 1.5,
    }))
    expect(await screen.findByText('Torrent settings saved.')).toBeInTheDocument()
  })
})
