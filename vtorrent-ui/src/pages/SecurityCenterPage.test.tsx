// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import SecurityCenterPage from './SecurityCenterPage'

const wallet = vi.hoisted(() => ({
  enable: vi.fn<() => Promise<{ uri: string; secret: string }>>(),
  verify: vi.fn<(code: string) => Promise<boolean>>(),
  disable: vi.fn<(code: string) => Promise<void>>(),
}))

vi.mock('../hooks/useWallet', () => ({
  useWallet: () => ({
    has2FA: false,
    enable2FA: wallet.enable,
    verify2FA: wallet.verify,
    disable2FA: wallet.disable,
  }),
}))

vi.mock('qrcode.react', () => ({
  default: () => <div data-testid="qr-code" />,
}))

describe('2FA enrollment', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    wallet.enable.mockResolvedValue({
      uri: 'otpauth://totp/vTorrent-Wallet?secret=JBSWY3DPEHPK3PXP',
      secret: 'JBSWY3DPEHPK3PXP',
    })
  })
  afterEach(cleanup)

  it('does not enable 2FA until the backend accepts a real code', async () => {
    wallet.verify.mockResolvedValueOnce(false).mockResolvedValueOnce(true)
    render(<SecurityCenterPage />)

    fireEvent.click(screen.getByRole('button', { name: 'Enable 2FA' }))
    expect(await screen.findByText('JBSWY3DPEHPK3PXP')).toBeInTheDocument()

    fireEvent.change(screen.getByPlaceholderText('000000'), { target: { value: '123456' } })
    fireEvent.click(screen.getByRole('button', { name: 'Confirm & Enable 2FA' }))

    expect(await screen.findByText(/Invalid code/)).toBeInTheDocument()
    expect(wallet.verify).toHaveBeenCalledWith('123456')
    expect(screen.queryByText('2FA Successfully Enabled')).not.toBeInTheDocument()

    fireEvent.change(screen.getByPlaceholderText('000000'), { target: { value: '654321' } })
    fireEvent.click(screen.getByRole('button', { name: 'Confirm & Enable 2FA' }))

    await waitFor(() => expect(screen.getByText('2FA Successfully Enabled')).toBeInTheDocument())
    expect(wallet.verify).toHaveBeenLastCalledWith('654321')
  })
})
