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

import { camel, isTauri, rpcDelete, rpcGet, rpcPost, tauriInvoke, RPC_BASE } from '../api'
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
    return tauriInvoke<TorrentSession[]>('get_torrent_sessions')
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

/**
 * Live staking status via WebSocket — replaces 5 s polling with instant push.
 * Subscribes to `staking_reward` events on `ws://RPC_BASE/ws`; falls back to
 * polling when WS is unavailable (Tauri or connection failure).
 */
export function useStakingStatus(_intervalMs = 8_000): {
  data: StakingStatus | null
  loading: boolean
  error: string | null
  refresh: () => void
} {
  const [data, setData] = useState<StakingStatus | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const wsRef = useRef<WebSocket | null>(null)
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const pollTimerRef = useRef<ReturnType<typeof setInterval> | null>(null)
  const mountedRef = useRef(true)

  const refresh = useCallback(async () => {
    try {
      const result = await fetchStakingStatus()
      if (!mountedRef.current) return
      setData(result)
      setError(null)
    } catch (e) {
      if (!mountedRef.current) return
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      if (mountedRef.current) setLoading(false)
    }
  }, [])

  const applyRewardEvent = useCallback(
    (_evData: Record<string, unknown>) => {
      void _evData
      setData(prev => {
        if (!prev) return prev
        return { ...prev, blocksStaked: prev.blocksStaked + 1, lastStakeTime: Math.floor(Date.now() / 1000) }
      })
      refresh()
    },
    [refresh],
  )

  useEffect(() => {
    mountedRef.current = true
    refresh()
    if (isTauri()) {
      pollTimerRef.current = setInterval(refresh, _intervalMs)
      return () => {
        mountedRef.current = false
        if (pollTimerRef.current) clearInterval(pollTimerRef.current)
      }
    }
    let closedIntentionally = false
    const getWsUrl = () => `${RPC_BASE.replace(/^http/, 'ws')}/ws`
    const connect = () => {
      if (closedIntentionally || !mountedRef.current) return
      try {
        const ws = new WebSocket(getWsUrl())
        wsRef.current = ws
        ws.onopen = () => {
          try {
            ws.send(JSON.stringify({ subscribe: ['staking_reward'] }))
          } catch {
            /* ignore */
          }
        }
        ws.onmessage = e => {
          try {
            const ev = JSON.parse((e as MessageEvent).data as string)
            const eventType: string | undefined = ev.event ?? ev.type ?? ev.Event
            const payload = ev.data ?? ev.payload ?? ev.Data ?? ev
            const normalized = (eventType ?? '').toLowerCase()
            if (normalized === 'staking_reward' || normalized === 'stakingreward') {
              if (payload && typeof payload === 'object') applyRewardEvent(payload as Record<string, unknown>)
              else refresh()
            }
          } catch {
            /* ignore */
          }
        }
        ws.onclose = () => {
          wsRef.current = null
          if (!closedIntentionally && mountedRef.current) {
            if (!pollTimerRef.current) pollTimerRef.current = setInterval(refresh, _intervalMs)
            reconnectTimerRef.current = setTimeout(() => {
              if (pollTimerRef.current) {
                clearInterval(pollTimerRef.current)
                pollTimerRef.current = null
              }
              connect()
            }, 3000)
          }
        }
      } catch {
        if (!pollTimerRef.current) pollTimerRef.current = setInterval(refresh, _intervalMs)
        reconnectTimerRef.current = setTimeout(connect, 3000)
      }
    }
    connect()
    return () => {
      mountedRef.current = false
      closedIntentionally = true
      if (wsRef.current) {
        try {
          wsRef.current.close()
        } catch {
          /* ignore */
        }
        wsRef.current = null
      }
      if (pollTimerRef.current) clearInterval(pollTimerRef.current)
      if (reconnectTimerRef.current) clearTimeout(reconnectTimerRef.current)
    }
  }, [refresh, _intervalMs, applyRewardEvent])

  return { data, loading, error, refresh }
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
  return camel(
    await rpcPost<unknown>('/api/v1/torrent/add', {
      source,
      source_type: sourceType,
      wallet_address: walletAddress,
    })
  ) as { sessionId: string; infoHash: string; name: string }
}

export async function removeTorrent(id: string): Promise<void> {
  if (isTauri()) {
    return tauriInvoke('remove_torrent', { id })
  }
  await rpcDelete(`/api/v1/torrent/${id}`)
}

// ─── DEX actions ─────────────────────────────────────────────────────────────

export async function placeDexOrder(req: {
  makerAddress: string
  makerBtcAddress?: string
  offerAmountSatoshis: number
  offerAsset: string
  requestAmountSatoshis: number
  requestAsset: string
  expirySecs: number
  passphrase: string
}): Promise<{ orderId: string; htlcAddress: string | null; hashLock: string }> {
  if (isTauri()) {
    return tauriInvoke('place_dex_order', req)
  }
  return camel(
    await rpcPost<unknown>('/api/v1/dex/order', {
      maker_address: req.makerAddress,
      maker_btc_address: req.makerBtcAddress ?? null,
      offer_amount_satoshis: req.offerAmountSatoshis,
      offer_asset: req.offerAsset,
      request_amount_satoshis: req.requestAmountSatoshis,
      request_asset: req.requestAsset,
      expiry_secs: req.expirySecs,
      passphrase: req.passphrase,
    })
  )
}

export async function cancelDexOrder(id: string): Promise<void> {
  if (isTauri()) {
    return tauriInvoke('cancel_dex_order', { id })
  }
  await rpcDelete(`/api/v1/dex/order/${id}`)
}

// ─── Swap lifecycle actions ───────────────────────────────────────────────────

export interface MatchOrderResult {
  orderId: string
  makerAddress: string
  vtrAmount: number
  targetAsset: string
  targetAmount: number
  hashLock: string
  expiry: number
  fundingTxid: string
}

export interface SwapActionResult {
  orderId: string
  txid: string
  status: string
}

/** Match an order as the taker, funding the maker's VTR HTLC. */
export async function matchDexOrder(req: {
  orderId: string
  takerAddress: string
  passphrase: string
  otpCode?: string
}): Promise<MatchOrderResult> {
  if (isTauri()) {
    return tauriInvoke<MatchOrderResult>('match_dex_order', req)
  }
  return camel(
    await rpcPost<unknown>('/api/v1/dex/match', {
      order_id: req.orderId,
      taker_address: req.takerAddress,
      passphrase: req.passphrase,
      otp_code: req.otpCode ?? null,
    })
  ) as MatchOrderResult
}

/** Fund the BTC side of the HTLC as the taker. */
export async function btcFund(req: {
  orderId: string
  btcRefundAddress: string
}): Promise<SwapActionResult> {
  if (isTauri()) {
    return tauriInvoke<SwapActionResult>('btc_fund', req)
  }
  return camel(
    await rpcPost<unknown>('/api/v1/swap/btc-fund', {
      order_id: req.orderId,
      btc_refund_address: req.btcRefundAddress,
    })
  ) as SwapActionResult
}

/** Claim VTR by revealing the preimage (taker). */
export async function vtrClaim(req: {
  orderId: string
  preimage: string
  takerWif: string
}): Promise<SwapActionResult> {
  if (isTauri()) {
    return tauriInvoke<SwapActionResult>('vtr_claim', req)
  }
  return camel(
    await rpcPost<unknown>('/api/v1/swap/vtr-claim', {
      order_id: req.orderId,
      preimage: req.preimage,
      taker_wif: req.takerWif,
    })
  ) as SwapActionResult
}

/** Claim BTC using the revealed preimage (maker). */
export async function btcClaim(orderId: string): Promise<SwapActionResult> {
  if (isTauri()) {
    return tauriInvoke<SwapActionResult>('btc_claim', { orderId })
  }
  return camel(
    await rpcPost<unknown>('/api/v1/swap/btc-claim', { order_id: orderId })
  ) as SwapActionResult
}

/** Refund either side after expiry. */
export async function swapRefund(orderId: string): Promise<SwapActionResult> {
  if (isTauri()) {
    return tauriInvoke<SwapActionResult>('swap_refund', { orderId })
  }
  return camel(
    await rpcPost<unknown>('/api/v1/swap/refund', { order_id: orderId })
  ) as SwapActionResult
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
