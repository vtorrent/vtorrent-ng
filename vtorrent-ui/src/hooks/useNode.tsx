/**
 * useNode — React hook for live vTorrent node data.
 *
 * When running inside Tauri (desktop), data is fetched via the Tauri IPC
 * bridge (`invoke`).  When running in a plain browser (dev / web mode), it
 * falls back to direct HTTP calls against the local RPC server on port 22525.
 *
 * All fetches are polled on a configurable interval so the UI stays fresh
 * without a full page reload.
 */

import { useState, useEffect, useCallback, useRef } from 'react'

// ─── Type declarations ────────────────────────────────────────────────────────

export interface NodeInfo {
  version: string
  network: string
  blockHeight: number
  bestBlockHash: string
  connections: number
  syncing: boolean
  /** Sync progress as a percentage (0.0–100.0). 100.0 means fully synced. */
  syncPercent: number
  /** Number of transactions currently in the mempool. */
  mempoolSize: number
  uptimeSecs: number
}

export interface TxRecord {
  txid: string
  blockHeight: number
  timestamp: number
  txType: string
  amountSatoshis: number
  display: string
}

export interface TorrentSession {
  id: string
  name: string
  infoHash: string
  state: string
  progress: number
  sizeBytes: number
  downloadedBytes: number
  uploadedBytes: number
  downloadSpeed: number
  uploadSpeed: number
  peerCount: number
  vtrEarnedSatoshis: number
  vtrPaidSatoshis: number
}

export interface DexOrder {
  id: string
  makerAddress: string
  offerAmountSatoshis: number
  offerAsset: string
  requestAmountSatoshis: number
  requestAsset: string
  rate: number
  status: string
  createdAt: number
  expiresAt: number
}

export interface StakingStatus {
  enabled: boolean
  stakingAddress: string | null
  eligibleUtxos: number
  totalStakingSatoshis: number
  expectedRewardPerDay: number
  lastStakeTime: number | null
  blocksStaked: number
}

export interface ClaimCheckResult {
  address: string
  claimableSatoshis: number
  display: string
  alreadyClaimed: boolean
}

export interface ClaimSubmitResult {
  txid: string
  claimedSatoshis: number
  recipientAddress: string
}

// ─── RPC base URL ─────────────────────────────────────────────────────────────

const RPC_BASE = 'http://127.0.0.1:22525'

/**
 * Optional RPC API key for browser (non-Tauri) mode.
 *
 * When the daemon is started with `--rpc-api-key`, sensitive endpoints require
 * the matching `X-API-Key` header. In the browser fallback there is no secure
 * way to store the key, so it is read from `localStorage` (set on the settings
 * screen or in devtools). Tauri IPC mode never sends the key.
 */
function getRpcApiKey(): string | null {
  if (isTauri()) return null
  try {
    return window.localStorage.getItem('vtorrent.rpc_api_key')
  } catch {
    return null
  }
}

function authHeaders(): Record<string, string> {
  const key = getRpcApiKey()
  return key ? { 'X-API-Key': key } : {}
}

// ─── Tauri detection ──────────────────────────────────────────────────────────

function isTauri(): boolean {
  return typeof (window as any).__TAURI_INTERNALS__ !== 'undefined'
}

async function tauriInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import('@tauri-apps/api/core')
  return invoke<T>(cmd, args)
}

// ─── Generic fetch helper ─────────────────────────────────────────────────────

async function rpcGet<T>(path: string): Promise<T> {
  const res = await fetch(`${RPC_BASE}${path}`, { headers: authHeaders() })
  if (!res.ok) throw new Error(`RPC ${path} → ${res.status}`)
  return res.json() as Promise<T>
}

async function rpcPost<T>(path: string, body: unknown): Promise<T> {
  const res = await fetch(`${RPC_BASE}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...authHeaders() },
    body: JSON.stringify(body),
  })
  if (!res.ok) {
    let msg = `RPC POST ${path} → ${res.status}`
    try {
      const err = await res.json()
      if (err.error) msg = err.error
    } catch { /* ignore */ }
    throw new Error(msg)
  }
  return res.json() as Promise<T>
}

// ─── Snake → camel key normaliser (shallow) ───────────────────────────────────

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function camel(obj: any): any {
  if (Array.isArray(obj)) return obj.map(camel)
  if (obj && typeof obj === 'object') {
    return Object.fromEntries(
      Object.entries(obj).map(([k, v]) => [
        k.replace(/_([a-z])/g, (_, c) => c.toUpperCase()),
        camel(v),
      ])
    )
  }
  return obj
}

// ─── Fetch functions ──────────────────────────────────────────────────────────

async function fetchNodeInfo(): Promise<NodeInfo> {
  if (isTauri()) {
    return tauriInvoke<NodeInfo>('get_node_info')
  }
  return camel(await rpcGet<unknown>('/api/v1/info')) as NodeInfo
}

async function fetchTransactions(limit = 20): Promise<TxRecord[]> {
  if (isTauri()) {
    return tauriInvoke<TxRecord[]>('get_transactions', { limit })
  }
  return camel(await rpcGet<unknown[]>(`/api/v1/wallet/transactions?limit=${limit}`)) as TxRecord[]
}

async function fetchTorrentSessions(): Promise<TorrentSession[]> {
  if (isTauri()) {
    return tauriInvoke<TorrentSession[]>('list_torrent_sessions')
  }
  return camel(await rpcGet<unknown[]>('/api/v1/torrent/sessions')) as TorrentSession[]
}

async function fetchDexOrders(): Promise<DexOrder[]> {
  if (isTauri()) {
    return tauriInvoke<DexOrder[]>('get_dex_orders')
  }
  return camel(await rpcGet<unknown[]>('/api/v1/dex/orders')) as DexOrder[]
}

async function fetchStakingStatus(): Promise<StakingStatus> {
  if (isTauri()) {
    return tauriInvoke<StakingStatus>('get_staking_status')
  }
  return camel(await rpcGet<unknown>('/api/v1/staking/status')) as StakingStatus
}

// ─── Generic polling hook ─────────────────────────────────────────────────────

function usePoll<T>(
  fetcher: () => Promise<T>,
  initial: T,
  intervalMs = 5000,
): { data: T; loading: boolean; error: string | null; refresh: () => void } {
  const [data, setData] = useState<T>(initial)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null)

  const run = useCallback(async () => {
    try {
      const result = await fetcher()
      setData(result)
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setLoading(false)
    }
  }, [fetcher])

  useEffect(() => {
    run()
    timerRef.current = setInterval(run, intervalMs)
    return () => {
      if (timerRef.current) clearInterval(timerRef.current)
    }
  }, [run, intervalMs])

  return { data, loading, error, refresh: run }
}

// ─── Public hooks ─────────────────────────────────────────────────────────────

/** Poll node info every 10 s. */
export function useNodeInfo(intervalMs = 10_000) {
  return usePoll<NodeInfo | null>(fetchNodeInfo, null, intervalMs)
}

/** Poll recent transactions every 15 s. */
export function useTransactions(limit = 20, intervalMs = 15_000) {
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const fetcher = useCallback(() => fetchTransactions(limit), [limit])
  return usePoll<TxRecord[]>(fetcher, [], intervalMs)
}

/** Poll torrent sessions every 5 s. */
export function useTorrentSessions(intervalMs = 5_000) {
  return usePoll<TorrentSession[]>(fetchTorrentSessions, [], intervalMs)
}

/** Poll DEX order book every 10 s. */
export function useDexOrders(intervalMs = 10_000) {
  return usePoll<DexOrder[]>(fetchDexOrders, [], intervalMs)
}

/** Poll staking status every 8 s. */
export function useStakingStatus(intervalMs = 8_000) {
  return usePoll<StakingStatus | null>(fetchStakingStatus, null, intervalMs)
}

// ─── Torrent actions ──────────────────────────────────────────────────────────

export async function addTorrent(
  source: string,
  sourceType: 'magnet' | 'file',
  walletAddress: string,
): Promise<{ sessionId: string; infoHash: string; name: string }> {
  if (isTauri()) {
    return tauriInvoke('add_torrent', { source, sourceType, walletAddress })
  }
  const res = await fetch(`${RPC_BASE}/api/v1/torrent/add`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ source, source_type: sourceType, wallet_address: walletAddress }),
  })
  if (!res.ok) throw new Error(`add_torrent → ${res.status}`)
  return camel(await res.json())
}

export async function removeTorrent(id: string): Promise<void> {
  if (isTauri()) {
    return tauriInvoke('remove_torrent', { id })
  }
  const res = await fetch(`${RPC_BASE}/api/v1/torrent/${id}`, { method: 'DELETE' })
  if (!res.ok) throw new Error(`remove_torrent → ${res.status}`)
}

// ─── DEX actions ─────────────────────────────────────────────────────────────

export async function placeDexOrder(req: {
  makerAddress: string
  offerAmountSatoshis: number
  offerAsset: string
  requestAmountSatoshis: number
  requestAsset: string
  expirySecs: number
  passphrase: string
}): Promise<{ orderId: string; htlcAddress: string; hashLock: string }> {
  if (isTauri()) {
    return tauriInvoke('place_dex_order', req)
  }
  const res = await fetch(`${RPC_BASE}/api/v1/dex/order`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      maker_address: req.makerAddress,
      offer_amount_satoshis: req.offerAmountSatoshis,
      offer_asset: req.offerAsset,
      request_amount_satoshis: req.requestAmountSatoshis,
      request_asset: req.requestAsset,
      expiry_secs: req.expirySecs,
      passphrase: req.passphrase,
    }),
  })
  if (!res.ok) throw new Error(`place_dex_order → ${res.status}`)
  return camel(await res.json())
}

export async function cancelDexOrder(id: string): Promise<void> {
  if (isTauri()) {
    return tauriInvoke('cancel_dex_order', { id })
  }
  const res = await fetch(`${RPC_BASE}/api/v1/dex/order/${id}`, { method: 'DELETE' })
  if (!res.ok) throw new Error(`cancel_dex_order → ${res.status}`)
}

// ─── Staking actions ──────────────────────────────────────────────────────────

/** Start staking on the given address. */
export async function startStaking(address: string): Promise<void> {
  if (isTauri()) {
    return tauriInvoke('start_staking', { address })
  }
  await rpcPost('/api/v1/staking/start', { address, passphrase: '' })
}

/** Stop staking. */
export async function stopStaking(): Promise<void> {
  if (isTauri()) {
    return tauriInvoke('stop_staking')
  }
  await rpcPost('/api/v1/staking/stop', {})
}

// ─── Legacy Claim actions ─────────────────────────────────────────────────────

/** Check the claimable balance for a legacy address. */
export async function checkLegacyClaim(legacyAddress: string): Promise<ClaimCheckResult> {
  if (isTauri()) {
    return tauriInvoke<ClaimCheckResult>('check_legacy_claim', { legacyAddress })
  }
  return camel(
    await rpcPost<unknown>('/api/v1/claim/check', { legacy_address: legacyAddress })
  ) as ClaimCheckResult
}

/** Submit a legacy claim transaction. */
export async function submitLegacyClaim(
  wifPrivateKey: string,
  recipientAddress: string,
): Promise<ClaimSubmitResult> {
  if (isTauri()) {
    return tauriInvoke<ClaimSubmitResult>('submit_legacy_claim', {
      wifPrivateKey,
      recipientAddress,
    })
  }
  return camel(
    await rpcPost<unknown>('/api/v1/claim/submit', {
      wif_private_key: wifPrivateKey,
      recipient_address: recipientAddress,
    })
  ) as ClaimSubmitResult
}
