import React, { createContext, useContext, useState, useCallback } from 'react'

export interface WalletKey {
  address: string
  label?: string
  isLegacyImport: boolean
  legacyAddress?: string
  balance: number  // in VTR satoshis
  createdAt: number
}

export interface WalletState {
  isUnlocked: boolean
  has2FA: boolean
  keys: WalletKey[]
  defaultAddress: string | null
  walletVersion: number | null
}

interface WalletContextType extends WalletState {
  unlock: (passphrase: string, otpCode?: string) => Promise<void>
  lock: () => void
  createWallet: (passphrase: string) => Promise<void>
  importLegacyWallet: (walletDatBase64: string, passphrase?: string) => Promise<ImportResult>
  enable2FA: () => Promise<{ uri: string; secret: string }>
  disable2FA: (otpCode: string) => Promise<void>
  generateAddress: (label?: string) => Promise<string>
  totalBalance: number
  legacyImportCount: number
}

export interface ImportResult {
  keysFound: number
  addresses: string[]
  hadEncryption: boolean
  had2FA: boolean
  claimableBalance: number
}

const WalletContext = createContext<WalletContextType | null>(null)

export function WalletProvider({ children }: { children: React.ReactNode }) {
  const [state, setState] = useState<WalletState>({
    isUnlocked: false,
    has2FA: false,
    keys: [],
    defaultAddress: null,
    walletVersion: null,
  })

  const unlock = useCallback(async (passphrase: string, otpCode?: string) => {
    // In the real Tauri app, this calls the Rust backend via invoke()
    // For the UI prototype, we simulate a successful unlock
    await new Promise(r => setTimeout(r, 800)) // simulate async
    setState(prev => ({ ...prev, isUnlocked: true }))
  }, [])

  const lock = useCallback(() => {
    setState(prev => ({ ...prev, isUnlocked: false }))
  }, [])

  const createWallet = useCallback(async (passphrase: string) => {
    await new Promise(r => setTimeout(r, 600))
    const mockAddress = 'V' + Array.from({ length: 33 }, () =>
      '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'[
        Math.floor(Math.random() * 58)
      ]
    ).join('')

    setState({
      isUnlocked: true,
      has2FA: false,
      keys: [{
        address: mockAddress,
        label: 'Primary Address',
        isLegacyImport: false,
        balance: 0,
        createdAt: Date.now(),
      }],
      defaultAddress: mockAddress,
      walletVersion: 1,
    })
  }, [])

  const importLegacyWallet = useCallback(async (
    walletDatBase64: string,
    passphrase?: string
  ): Promise<ImportResult> => {
    // Simulate the Rust backend parsing wallet.dat
    await new Promise(r => setTimeout(r, 1500))

    const mockAddresses = Array.from({ length: 3 }, (_, i) =>
      'V' + Array.from({ length: 33 }, () =>
        '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'[
          Math.floor(Math.random() * 58)
        ]
      ).join('')
    )

    const mockKeys: WalletKey[] = mockAddresses.map((addr, i) => ({
      address: addr,
      label: `Imported Key #${i + 1}`,
      isLegacyImport: true,
      legacyAddress: addr,
      balance: Math.floor(Math.random() * 100000000),
      createdAt: Date.now(),
    }))

    setState({
      isUnlocked: true,
      has2FA: false,
      keys: mockKeys,
      defaultAddress: mockAddresses[0],
      walletVersion: 1,
    })

    return {
      keysFound: mockKeys.length,
      addresses: mockAddresses,
      hadEncryption: !!passphrase,
      had2FA: false,
      claimableBalance: mockKeys.reduce((sum, k) => sum + k.balance, 0),
    }
  }, [])

  const enable2FA = useCallback(async () => {
    await new Promise(r => setTimeout(r, 300))
    const mockSecret = 'JBSWY3DPEHPK3PXP'
    const uri = `otpauth://totp/vTorrent-Wallet?secret=${mockSecret}&issuer=vTorrent`
    setState(prev => ({ ...prev, has2FA: true }))
    return { uri, secret: mockSecret }
  }, [])

  const disable2FA = useCallback(async (otpCode: string) => {
    await new Promise(r => setTimeout(r, 300))
    setState(prev => ({ ...prev, has2FA: false }))
  }, [])

  const generateAddress = useCallback(async (label?: string) => {
    await new Promise(r => setTimeout(r, 300))
    const newAddr = 'V' + Array.from({ length: 33 }, () =>
      '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'[
        Math.floor(Math.random() * 58)
      ]
    ).join('')

    setState(prev => ({
      ...prev,
      keys: [...prev.keys, {
        address: newAddr,
        label: label || `Address #${prev.keys.length + 1}`,
        isLegacyImport: false,
        balance: 0,
        createdAt: Date.now(),
      }]
    }))
    return newAddr
  }, [])

  const totalBalance = state.keys.reduce((sum, k) => sum + k.balance, 0)
  const legacyImportCount = state.keys.filter(k => k.isLegacyImport).length

  return (
    <WalletContext.Provider value={{
      ...state,
      unlock,
      lock,
      createWallet,
      importLegacyWallet,
      enable2FA,
      disable2FA,
      generateAddress,
      totalBalance,
      legacyImportCount,
    }}>
      {children}
    </WalletContext.Provider>
  )
}

export function useWallet() {
  const ctx = useContext(WalletContext)
  if (!ctx) throw new Error('useWallet must be used within WalletProvider')
  return ctx
}

export function formatVTR(satoshis: number): string {
  return (satoshis / 100_000_000).toFixed(8).replace(/\.?0+$/, '') + ' VTR'
}
