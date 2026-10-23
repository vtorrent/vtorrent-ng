// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import RestoreBackupForm from './RestoreBackupForm'

const wallet = vi.hoisted(() => ({
  verify: vi.fn(),
  restore: vi.fn(),
}))

vi.mock('../hooks/useWallet', () => ({
  useWallet: () => ({
    verifyBackup: wallet.verify,
    restoreBackup: wallet.restore,
  }),
}))

vi.mock('../api', async importOriginal => ({
  ...await importOriginal<typeof import('../api')>(),
  fileToBase64: vi.fn().mockResolvedValue('YmFja3Vw'),
}))

const preview = {
  walletAddress: 'VTestWalletAddress',
  has2FA: false,
  fileCount: 3,
  createdAt: 1,
  files: ['wallet.vtr', 'wallet.cold_stake.json', 'wallet.swaps/order.json'],
}

describe('restore backup form', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    wallet.verify.mockResolvedValue(preview)
    wallet.restore.mockResolvedValue(preview)
  })
  afterEach(cleanup)

  it('verifies the backup before enabling restore', async () => {
    const onRestored = vi.fn()
    const { container } = render(<RestoreBackupForm onRestored={onRestored} />)

    const fileInput = container.querySelector('input[type="file"]') as HTMLInputElement
    fireEvent.change(fileInput, {
      target: { files: [new File(['backup'], 'test.vtrbackup')] },
    })
    await screen.findByText('test.vtrbackup')

    fireEvent.change(
      screen.getByPlaceholderText('Passphrase used when the backup was created'),
      { target: { value: 'password' } }
    )
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Verify backup' })).toBeEnabled()
    )
    fireEvent.click(screen.getByRole('button', { name: 'Verify backup' }))

    await waitFor(() => expect(wallet.verify).toHaveBeenCalledWith(
      'YmFja3Vw',
      'password',
      undefined
    ))
    expect(await screen.findByText('VTestWalletAddress')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Restore backup' })).toBeDisabled()

    fireEvent.click(screen.getByRole('checkbox'))
    fireEvent.click(screen.getByRole('button', { name: 'Restore backup' }))

    await waitFor(() => expect(wallet.restore).toHaveBeenCalledWith(
      'YmFja3Vw',
      'password',
      undefined,
      true
    ))
    expect(onRestored).toHaveBeenCalledWith({
      preview,
      passphrase: 'password',
      otpCode: undefined,
    })
  })
})
