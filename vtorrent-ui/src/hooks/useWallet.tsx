import React, { createContext, useContext, useState, useCallback, useEffect } from 'react'
import {
  btcRecoveryKind,
  formatInvokeError,
  isTauri,
  tauriInvoke as invokeTauri,
  type BtcRecoveryKind,
} from '../api'

// ─── Tauri IPC bridge ─────────────────────────────────────────────────────────
// In the Tauri desktop app, `invoke` calls the Rust backend directly.
// In browser dev mode (Vite), we fall back to mock implementations.

type InvokeFn = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>

const tauriInvoke: InvokeFn | null = isTauri()
  ? (cmd, args) => invokeTauri(cmd, args)
  : null

class NodeStartError extends Error {
  readonly btcRecovery: BtcRecoveryKind | null
  readonly cause?: unknown

  constructor(message: string, cause: unknown) {
    super(message)
    this.name = 'NodeStartError'
    this.cause = cause
    this.btcRecovery = btcRecoveryKind(cause)
  }
}

function nodeStartError(message: string, cause: unknown): Error {
  return new NodeStartError(message, cause)
}

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

export interface BackupInfo {
  path: string
  size: number
  fileCount: number
  createdAt: number
}

export interface BackupPreview {
  walletAddress: string
  has2FA: boolean
  fileCount: number
  createdAt: number
  files: string[]
}

export interface VtrQuote {
  quoteId: string
  fromAddress: string
  toAddress: string
  amountSatoshis: number
  feeSatoshis: number
  totalDebitSatoshis: number
  changeSatoshis: number
  inputCount: number
  feeRate: number
  network: string
}

export interface WalletState {
  isUnlocked: boolean
  has2FA: boolean
  keys: WalletKey[]
  defaultAddress: string | null
  walletVersion: number | null
}

interface WalletContextType extends WalletState {
  unlock: (passphrase: string, otpCode?: string, net?: { network?: string, seeds?: string[] }) => Promise<void>
  startNode: (net?: { network?: string, seeds?: string[] }) => Promise<void>
  lock: () => void
  createWallet: (passphrase: string, net?: { network?: string, seeds?: string[] }) => Promise<void>
  importLegacyWallet: (walletDatBase64: string, legacyPassphrase: string | undefined, newPassphrase: string) => Promise<ImportResult>
  enable2FA: (passphrase: string) => Promise<{ uri: string; secret: string }>
  verify2FA: (code: string, passphrase: string) => Promise<boolean>
  disable2FA: (otpCode: string, passphrase: string) => Promise<void>
  generateAddress: (label?: string) => Promise<string>
  quoteVtr: (toAddress: string, amountSatoshis: number) => Promise<VtrQuote>
  sendVtr: (
    toAddress: string,
    amountSatoshis: number,
    quoteId: string | undefined,
    passphrase: string,
    otpCode?: string
  ) => Promise<string>
  createBackup: (passphrase: string, otpCode?: string) => Promise<BackupInfo>
  verifyBackup: (backupBase64: string, passphrase: string, otpCode?: string) => Promise<BackupPreview>
  restoreBackup: (
    backupBase64: string,
    passphrase: string,
    otpCode: string | undefined,
    confirmOverwrite: boolean
  ) => Promise<BackupPreview>
  repairBtcState: (
    passphrase: string,
    otpCode: string | undefined,
    attestNoPriorBtcHistory: boolean
  ) => Promise<string>
  totalBalance: number
  legacyImportCount: number
}

export interface ImportResult {
  keysFound: number
  addresses: string[]
  defaultAddress: string
  hadEncryption: boolean
  had2FA: boolean
  claimableBalance: number
  nodeError?: string
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
  default_address: string
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
        default_address: addrs[0],
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
    case 'quote_vtr': {
      const amount = Number(args?.amountSatoshis ?? 0)
      const fee = 1_000
      return {
        fromAddress: mockAddress(),
        toAddress: String(args?.toAddress ?? ''),
        amountSatoshis: amount,
        feeSatoshis: fee,
        totalDebitSatoshis: amount + fee,
        changeSatoshis: 0,
        inputCount: 1,
        feeRate: 10,
        network: 'vtorrent-mainnet',
      } as T
    }
    case 'send_vtr': {
      // Mock: return a fake txid
      const fakeTxid = Array.from({ length: 64 }, () =>
        '0123456789abcdef'[Math.floor(Math.random() * 16)]
      ).join('')
      return fakeTxid as T
    }
    case 'create_backup':
      return {
        path: '/tmp/vtorrent-backup.vtrbackup',
        size: 4096,
        fileCount: 3,
        createdAt: Math.floor(Date.now() / 1000),
      } as T
    case 'verify_backup':
    case 'restore_backup':
      return {
        walletAddress: mockAddress(),
        has2FA: false,
        fileCount: 3,
        createdAt: Math.floor(Date.now() / 1000),
        files: ['wallet.vtr', 'wallet.cold_stake.json', 'wallet.swaps/order.json'],
      } as T
    case 'start_node':
    case 'stop_node':
      // Mock: no-op in browser dev mode
      return undefined as T
    case 'probe_seed_peers':
      // Mock: no local peers in browser dev mode
      return [] as T
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

  const startNode = useCallback(async (net?: { network?: string, seeds?: string[] }) => {
    await invoke('start_node', {
      network: net?.network ?? 'mainnet',
      seeds: net?.seeds ?? [],
    })
  }, [])

  const unlock = useCallback(async (passphrase: string, otpCode?: string, net?: { network?: string, seeds?: string[] }) => {
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
    // Start the P2P node after the wallet is unlocked. A failure is surfaced
    // with a typed BTC-recovery classification so the welcome page can offer
    // the authenticated repair flow instead of reporting a passphrase error.
    try {
      await startNode(net)
    } catch (e) {
      throw nodeStartError(
        `Wallet unlocked, but the node failed to start: ${formatInvokeError(e, 'start_node failed')}`,
        e
      )
    }
  }, [startNode])

  const lock = useCallback(async () => {
    await invoke('lock_wallet')
    setState(prev => ({ ...prev, isUnlocked: false }))
  }, [])

  const createWallet = useCallback(async (passphrase: string, net?: { network?: string, seeds?: string[] }) => {
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
        network: net?.network ?? 'mainnet',
        seeds: net?.seeds ?? [],
      })
    } catch (e) {
      throw nodeStartError(
        `Wallet created, but the node failed to start: ${formatInvokeError(e, 'start_node failed')}`,
        e
      )
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

    const defaultAddr = result.default_address

    setState({
      isUnlocked: true,
      has2FA: false,
      keys,
      defaultAddress: defaultAddr,
      walletVersion: 2,
    })

    // Register the imported WIF with the RPC hot wallet so send_vtr works.
    // Also start the node so the UI gets live data immediately. A node start
    // failure is surfaced on the result screen instead of being swallowed.
    let nodeError: string | undefined
    try {
      await invoke('start_node', {
        stakingAddress: defaultAddr,
        dataDir: null,
      })
    } catch (e) {
      nodeError = formatInvokeError(e, 'Node failed to start after import')
    }

    return {
      keysFound: result.keys_found,
      addresses: result.addresses,
      defaultAddress: defaultAddr,
      hadEncryption: result.had_encryption,
      had2FA: result.had_2fa,
      claimableBalance: result.claimable_balance,
      nodeError,
    }
  }, [])

  const enable2FA = useCallback(async (passphrase: string) => {
    const result = await invoke<TauriEnable2FAResult>('enable_2fa', { passphrase })
    return { uri: result.uri, secret: result.secret }
  }, [])

  const verify2FA = useCallback(async (code: string, passphrase: string) => {
    const valid = await invoke<boolean>('verify_2fa', { code, passphrase })
    if (valid) setState(prev => ({ ...prev, has2FA: true }))
    return valid
  }, [])

  const disable2FA = useCallback(async (otpCode: string, passphrase: string) => {
    await invoke('disable_2fa', { code: otpCode, passphrase })
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

  const quoteVtr = useCallback(async (toAddress: string, amountSatoshis: number): Promise<VtrQuote> => {
    return invoke<VtrQuote>('quote_vtr', { toAddress, amountSatoshis })
  }, [])

  const sendVtr = useCallback(async (
    toAddress: string,
    amountSatoshis: number,
    quoteId: string | undefined,
    passphrase: string,
    otpCode?: string
  ): Promise<string> => {
    const txid = await invoke<string>('send_vtr', {
      toAddress,
      amountSatoshis,
      quoteId: quoteId ?? null,
      passphrase,
      otpCode: otpCode ?? null,
    })
    return txid
  }, [])

  const createBackup = useCallback(async (passphrase: string, otpCode?: string): Promise<BackupInfo> => {
    return invoke<BackupInfo>('create_backup', {
      passphrase,
      otpCode: otpCode ?? null,
    })
  }, [])

  const verifyBackup = useCallback(async (
    backupBase64: string,
    passphrase: string,
    otpCode?: string
  ): Promise<BackupPreview> => {
    return invoke<BackupPreview>('verify_backup', {
      backupBase64,
      passphrase,
      otpCode: otpCode ?? null,
    })
  }, [])

  const restoreBackup = useCallback(async (
    backupBase64: string,
    passphrase: string,
    otpCode: string | undefined,
    confirmOverwrite: boolean
  ): Promise<BackupPreview> => {
    return invoke<BackupPreview>('restore_backup', {
      backupBase64,
      passphrase,
      otpCode: otpCode ?? null,
      confirmOverwrite,
    })
  }, [])

  const repairBtcState = useCallback(async (
    passphrase: string,
    otpCode: string | undefined,
    attestNoPriorBtcHistory: boolean
  ) => {
    return invoke<string>('repair_btc_state', {
      passphrase,
      otpCode: otpCode ?? null,
      attestNoPriorBtcHistory,
    })
  }, [])

  const totalBalance = state.keys.reduce((sum, k) => sum + k.balance, 0)
  const legacyImportCount = state.keys.filter(k => k.isLegacyImport).length

  return (
    <WalletContext.Provider value={{
      ...state,
      unlock,
      startNode,
      lock,
      createWallet,
      importLegacyWallet,
      enable2FA,
      verify2FA,
      disable2FA,
      generateAddress,
      quoteVtr,
      sendVtr,
      createBackup,
      verifyBackup,
      restoreBackup,
      repairBtcState,
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
