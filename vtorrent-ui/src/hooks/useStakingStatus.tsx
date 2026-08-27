/**
 * useStakingStatus — WS push for live staking dashboard.
 *
 * Replaces the previous 5-8 s polling loop with a WebSocket subscription
 * to the RPC `staking_reward` event stream (see `vtorrent-rpc/src/ws.rs`).
 * On receipt of a `StakingReward` event the hook refreshes the full
 * staking status via HTTP (or Tauri IPC) so the dashboard updates in
 * <500 ms instead of waiting for the next poll interval.
 *
 * When WebSocket is unavailable (Tauri mode, connection failure) the hook
 * transparently falls back to polling so the UI never stalls.
 */

import { useState, useEffect, useCallback, useRef } from 'react'

// ─── Types ────────────────────────────────────────────────────────────────────

export interface StakingStatus {
  enabled: boolean
  stakingAddress: string | null
  eligibleUtxos: number
  totalStakingSatoshis: number
  expectedRewardPerDay: number
  lastStakeTime: number | null
  blocksStaked: number
}

// ─── RPC helpers ──────────────────────────────────────────────────────────────

const RPC_BASE = 'http://127.0.0.1:22525'

function getWsUrl(): string {
  // RPC_BASE is http://host:port → ws://host:port/ws
  return `${RPC_BASE.replace(/^http/, 'ws')}/ws`
}

function isTauri(): boolean {
  return typeof (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ !== 'undefined'
}

async function tauriInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import('@tauri-apps/api/core')
  return invoke<T>(cmd, args)
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function camel(obj: any): any {
  if (Array.isArray(obj)) return obj.map(camel)
  if (obj && typeof obj === 'object') {
    return Object.fromEntries(
      Object.entries(obj).map(([k, v]) => [
        k.replace(/_([a-z])/g, (_: string, c: string) => c.toUpperCase()),
        camel(v),
      ])
    )
  }
  return obj
}

async function fetchStakingStatus(): Promise<StakingStatus> {
  if (isTauri()) {
    return tauriInvoke<StakingStatus>('get_staking_status')
  }
  const res = await fetch(`${RPC_BASE}/api/v1/staking/status`)
  if (!res.ok) throw new Error(`RPC /api/v1/staking/status → ${res.status}`)
  return camel(await res.json()) as StakingStatus
}

// ─── Hook ─────────────────────────────────────────────────────────────────────

/**
 * Live staking status via WebSocket.
 *
 * @param _pollIntervalMs - kept for backwards compatibility; used only as
 *                          fallback poll interval when WS is unavailable.
 *                          Defaults to 8 s to match the previous poll cadence.
 */
export function useStakingStatus(_pollIntervalMs = 8_000): {
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

  // Optimistic patch from a StakingReward WS payload
  const applyRewardEvent = useCallback(
    (evData: { block_height?: number; reward_sats?: number; address?: string; blockHeight?: number; rewardSats?: number }) => {
      // Patch local state for instant feedback, then fetch authoritative state.
      setData(prev => {
        if (!prev) return prev
        const ts = Math.floor(Date.now() / 1000)
        // Prefer snake_case (wire) but accept camelCase.
        const height = evData.block_height ?? evData.blockHeight
        void height
        return {
          ...prev,
          blocksStaked: prev.blocksStaked + 1,
          lastStakeTime: ts,
        }
      })
      // Background refresh to reconcile with server truth.
      refresh()
    },
    [refresh],
  )

  useEffect(() => {
    mountedRef.current = true
    // Initial fetch so UI is populated before WS connects.
    refresh()

    // Tauri mode: keep polling (IPC, no WS).
    if (isTauri()) {
      pollTimerRef.current = setInterval(refresh, _pollIntervalMs)
      return () => {
        mountedRef.current = false
        if (pollTimerRef.current) clearInterval(pollTimerRef.current)
      }
    }

    let closedIntentionally = false

    const connect = () => {
      if (closedIntentionally || !mountedRef.current) return
      try {
        const ws = new WebSocket(getWsUrl())
        wsRef.current = ws

        ws.onopen = () => {
          // Subscribe to staking_reward events per ws.rs protocol.
          try {
            ws.send(JSON.stringify({ subscribe: ['staking_reward'] }))
          } catch {
            /* ignore */
          }
        }

        ws.onmessage = e => {
          try {
            const ev = JSON.parse(e.data as string)
            // ws.rs uses {event:"staking_reward", data:{...}}
            // Plan snippet uses {type:"StakingReward", payload:{...}} — handle both.
            const eventType: string | undefined = ev.event ?? ev.type ?? ev.Event
            const payload = ev.data ?? ev.payload ?? ev.Data ?? ev
            const normalized = (eventType ?? '').toLowerCase()
            if (normalized === 'staking_reward' || normalized === 'stakingreward') {
              // WS payload is {block_height, reward_sats, address} — not full StakingStatus.
              // Patch + refresh rather than replacing status directly.
              if (payload && typeof payload === 'object') {
                applyRewardEvent(payload as Record<string, unknown>)
              } else {
                refresh()
              }
            }
          } catch {
            /* malformed message — ignore */
          }
        }

        ws.onerror = () => {
          // Let onclose handle reconnection.
        }

        ws.onclose = () => {
          wsRef.current = null
          if (!closedIntentionally && mountedRef.current) {
            // Fallback poll while WS is down, then try to reconnect.
            if (!pollTimerRef.current) {
              pollTimerRef.current = setInterval(refresh, _pollIntervalMs)
            }
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
        // WS construction failed (e.g. no network) — fall back to polling.
        if (!pollTimerRef.current) {
          pollTimerRef.current = setInterval(refresh, _pollIntervalMs)
        }
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
  }, [refresh, _pollIntervalMs, applyRewardEvent])

  return { data, loading, error, refresh }
}

export default useStakingStatus
