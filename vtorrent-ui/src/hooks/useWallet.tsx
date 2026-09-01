import React, { createContext, useContext, useState, useCallback, useEffect } from 'react'
import { isTauri, tauriInvoke as invokeTauri } from '../api'

// ─── Tauri IPC bridge ─────────────────────────────────────────────────────────
// In the Tauri desktop app, `invoke` calls the Rust backend directly.
// In browser dev mode (Vite), we fall back to mock implementations.

type InvokeFn = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>

const tauriInvoke: InvokeFn | null = isTauri()
  ? (cmd, args) => invokeTauri(cmd, args)
  : null

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (tauriInvoke) {
    return tauriInvoke(cmd, args) as Promise<T>
  }
  // Browser dev mode: use mock
  return mockInvoke<T>(cmd, args)
}

// ─── Types ────────────────────────────────────────────────────────────────────

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
  importLegacyWallet: (walletDatBase64: string, legacyPassphrase: string | undefined, newPassphrase: string) => Promise<ImportResult>
  enable2FA: () => Promise<{ uri: string; secret: string }>
  disable2FA: (otpCode: string) => Promise<void>
  generateAddress: (label?: string) => Promise<string>
  sendVtr: (toAddress: string, amountSatoshis: number) => Promise<string>
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

// ─── Tauri response types (snake_case from Rust) ──────────────────────────────

interface TauriWalletInfo {
  is_unlocked: boolean
  has_2fa: boolean
  address_count: number
  default_address: string | null
  wallet_version: number
}

interface TauriImportResult {
  keys_found: number
  addresses: string[]
  had_encryption: boolean
  had_2fa: boolean
  claimable_balance: number
}

interface TauriAddressInfo {
  address: string
  label: string
  balance: number
  is_legacy_import: boolean
}

interface TauriEnable2FAResult {
  uri: string
  secret: string
  qr_data: string
}

// ─── Mock implementations (browser dev mode only) ────────────────────────────

function mockAddress(): string {
  return 'V' + Array.from({ length: 33 }, () =>
    '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'[
      Math.floor(Math.random() * 58)
    ]
  ).join('')
}

async function mockInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  await new Promise(r => setTimeout(r, 400 + Math.random() * 400))

  switch (cmd) {
    case 'create_wallet': {
      const addr = mockAddress()
      return {
        is_unlocked: true, has_2fa: false, address_count: 1,
        default_address: addr, wallet_version: 2,
      } as T
    }
    case 'open_wallet': {
      const addr = mockAddress()
      return {
        is_unlocked: true, has_2fa: false, address_count: 1,
        default_address: addr, wallet_version: 2,
      } as T
    }
    case 'lock_wallet':
      return undefined as T
    case 'get_wallet_info':
      return {
        is_unlocked: false, has_2fa: false, address_count: 0,
        default_address: null, wallet_version: 0,
      } as T
    case 'import_legacy_wallet': {
      const addrs = Array.from({ length: 3 }, mockAddress)
      return {
        keys_found: addrs.length,
        addresses: addrs,
        had_encryption: !!(args?.passphrase),
        had_2fa: false,
        claimable_balance: Math.floor(Math.random() * 10_000_000_000),
      } as T
    }
    case 'generate_address': {
      const addr = mockAddress()
      return {
        address: addr,
        label: (args?.label as string) || 'New Address',
        balance: 0,
        is_legacy_import: false,
      } as T
    }
    case 'get_addresses': {
      return [] as T
    }
    case 'enable_2fa': {
      const secret = 'JBSWY3DPEHPK3PXP'
      const uri = `otpauth://totp/vTorrent-Wallet?secret=${secret}&issuer=vTorrent`
      return { uri, secret, qr_data: uri } as T
    }
    case 'verify_2fa':
      return true as T
    case 'disable_2fa':
      return undefined as T
    case 'send_vtr': {
      // Mock: return a fake txid
      const fakeTxid = Array.from({ length: 64 }, () =>
        '0123456789abcdef'[Math.floor(Math.random() * 16)]
      ).join('')
      return fakeTxid as T
    }
    case 'start_node':
    case 'stop_node':
      // Mock: no-op in browser dev mode
      return undefined as T
    default:
      throw new Error(`Unknown mock command: ${cmd}`)
  }
}

// ─── Wallet data directory helper ─────────────────────────────────────────────

async function getWalletPath(): Promise<string> {
  if (isTauri()) {
    const { appDataDir, join } = await import('@tauri-apps/api/path')
    return join(await appDataDir(), 'wallet.vtr')
  }
  return 'wallet.vtr'
}

function mapAddress(a: TauriAddressInfo): WalletKey {
  return {
    address: a.address,
    label: a.label,
    isLegacyImport: a.is_legacy_import,
    legacyAddress: a.is_legacy_import ? a.address : undefined,
    balance: a.balance,
    createdAt: Date.now(),
  }
}

// ─── Context & Provider ───────────────────────────────────────────────────────

const WalletContext = createContext<WalletContextType | null>(null)

export function WalletProvider({ children }: { children: React.ReactNode }) {
  const [state, setState] = useState<WalletState>({
    isUnlocked: false,
    has2FA: false,
    keys: [],
    defaultAddress: null,
    walletVersion: null,
  })

  useEffect(() => {
    if (!state.isUnlocked || !isTauri()) return
    let cancelled = false
    const refresh = async () => {
      try {
        const keys = (await invoke<TauriAddressInfo[]>('get_addresses')).map(mapAddress)
        if (!cancelled) setState(prev => ({ ...prev, keys }))
      } catch {
        // The node may still be starting; the next refresh will retry.
      }
    }
    void refresh()
    const timer = window.setInterval(refresh, 10_000)
    return () => {
      cancelled = true
      window.clearInterval(timer)
    }
  }, [state.isUnlocked])

  const unlock = useCallback(async (passphrase: string, otpCode?: string) => {
    const info = await invoke<TauriWalletInfo>('open_wallet', {
      walletPath: await getWalletPath(),
      passphrase,
      otpCode: otpCode ?? null,
    })
    const keys = (await invoke<TauriAddressInfo[]>('get_addresses')).map(mapAddress)
    setState(prev => ({
      ...prev,
      isUnlocked: info.is_unlocked,
      has2FA: info.has_2fa,
      defaultAddress: info.default_address,
      walletVersion: info.wallet_version,
      keys,
    }))
    // Start the P2P node in the background after the wallet is unlocked.
    // Errors are non-fatal — the UI still works without a running node.
    try {
      await invoke('start_node', {
        stakingAddress: info.default_address ?? null,
        dataDir: null,
      })
    } catch (e) {
      console.warn('start_node failed (may already be running):', e)
    }
  }, [])

  const lock = useCallback(async () => {
    await invoke('lock_wallet')
    setState(prev => ({ ...prev, isUnlocked: false }))
  }, [])

  const createWallet = useCallback(async (passphrase: string) => {
    const info = await invoke<TauriWalletInfo>('create_wallet', {
      passphrase,
      walletPath: await getWalletPath(),
    })
    const keys = (await invoke<TauriAddressInfo[]>('get_addresses')).map(mapAddress)
    setState({
      isUnlocked: info.is_unlocked,
      has2FA: info.has_2fa,
      keys,
      defaultAddress: info.default_address,
      walletVersion: info.wallet_version,
    })
    try {
      await invoke('start_node', {
        stakingAddress: info.default_address ?? null,
        dataDir: null,
      })
    } catch (e) {
      console.warn('start_node failed after wallet creation:', e)
    }
  }, [])

  const importLegacyWallet = useCallback(async (
    walletDatBase64: string,
    legacyPassphrase?: string,
    newPassphrase?: string
  ): Promise<ImportResult> => {
    const result = await invoke<TauriImportResult>('import_legacy_wallet', {
      walletDatBase64,
      passphrase: legacyPassphrase ?? null,
      newWalletPassphrase: newPassphrase ?? legacyPassphrase ?? '',
      newWalletPath: await getWalletPath(),
    })

    // Fetch the full address list from the backend
    const addrInfos = await invoke<TauriAddressInfo[]>('get_addresses')
    const keys: WalletKey[] = addrInfos.map(mapAddress)

    const defaultAddr = result.addresses[0] ?? null

    setState({
      isUnlocked: true,
      has2FA: false,
      keys,
      defaultAddress: defaultAddr,
      walletVersion: 2,
    })

    // Register the imported WIF with the RPC hot wallet so send_vtr works.
    // Also start the node so the UI gets live data immediately.
    try {
      await invoke('start_node', {
        stakingAddress: defaultAddr,
        dataDir: null,
      })
    } catch (e) {
      console.warn('start_node failed after import (may already be running):', e)
    }

    return {
      keysFound: result.keys_found,
      addresses: result.addresses,
      hadEncryption: result.had_encryption,
      had2FA: result.had_2fa,
      claimableBalance: result.claimable_balance,
    }
  }, [])

  const enable2FA = useCallback(async () => {
    const result = await invoke<TauriEnable2FAResult>('enable_2fa')
    setState(prev => ({ ...prev, has2FA: true }))
    return { uri: result.uri, secret: result.secret }
  }, [])

  const disable2FA = useCallback(async (otpCode: string) => {
    await invoke('disable_2fa', { code: otpCode })
    setState(prev => ({ ...prev, has2FA: false }))
  }, [])

  const generateAddress = useCallback(async (label?: string) => {
    const info = await invoke<TauriAddressInfo>('generate_address', {
      label: label ?? null,
    })
    setState(prev => ({
      ...prev,
      keys: [...prev.keys, {
        address: info.address,
        label: info.label,
        isLegacyImport: false,
        balance: 0,
        createdAt: Date.now(),
      }]
    }))
    return info.address
  }, [])

  const sendVtr = useCallback(async (toAddress: string, amountSatoshis: number): Promise<string> => {
    const txid = await invoke<string>('send_vtr', {
      toAddress,
      amountSatoshis,
    })
    return txid
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
      sendVtr,
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
