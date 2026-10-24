// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router-dom'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import BackupRestorePanel from './BackupRestorePanel'

const wallet = vi.hoisted(() => ({
  create: vi.fn(),
  verify: vi.fn(),
  restore: vi.fn(),
  lock: vi.fn(),
}))

vi.mock('../hooks/useWallet', () => ({
  useWallet: () => ({
    createBackup: wallet.create,
    verifyBackup: wallet.verify,
    restoreBackup: wallet.restore,
    has2FA: false,
    lock: wallet.lock,
  }),
}))

describe('backup panel', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    wallet.create.mockResolvedValue({
      path: '/tmp/vtorrent-backup.vtrbackup',
      size: 4096,
      fileCount: 3,
      createdAt: 1,
    })
  })
  afterEach(cleanup)

  it('creates an encrypted backup and shows its path', async () => {
    render(
      <MemoryRouter>
        <BackupRestorePanel />
      </MemoryRouter>
    )

    fireEvent.change(screen.getByPlaceholderText('Encrypts the backup'), {
      target: { value: 'password' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Create backup' }))

    await waitFor(() => expect(wallet.create).toHaveBeenCalledWith('password', undefined))
    expect(await screen.findByText('/tmp/vtorrent-backup.vtrbackup')).toBeInTheDocument()
    expect(screen.getByText(/3 files/)).toBeInTheDocument()
    expect(screen.getByText(/2FA protects app unlock, not the backup/)).toBeInTheDocument()
  })
})
