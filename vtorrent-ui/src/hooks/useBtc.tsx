import { useState, useEffect, useCallback } from 'react'

// ─── Tauri detection ──────────────────────────────────────────────────────────

function isTauri(): boolean {
  return typeof (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ !== 'undefined'
}

async function tauriInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import('@tauri-apps/api/core')
  return invoke<T>(cmd, args)
}

// ─── Types ────────────────────────────────────────────────────────────────────

export interface BtcStatus {
  initialized: boolean
  balanceSatoshis: number
  address: string | null
  bestHeight: number
  synced: boolean
}

const RPC_BASE = 'http://127.0.0.1:22525'

function camel<T>(obj: unknown): T {
  if (Array.isArray(obj)) return obj.map(camel) as unknown as T
  if (obj && typeof obj === 'object') {
    const out: Record<string, unknown> = {}
    for (const [k, v] of Object.entries(obj as Record<string, unknown>)) {
      const key = k.replace(/_([a-z])/g, (_, c) => c.toUpperCase())
      out[key] = camel(v)
    }
    return out as T
  }
  return obj as T
}

// ─── Fetch functions ──────────────────────────────────────────────────────────

async function fetchBtcStatus(): Promise<BtcStatus> {
  if (isTauri()) {
    return tauriInvoke<BtcStatus>('get_btc_status')
  }
  const res = await fetch(`${RPC_BASE}/api/v1/btc/status`)
  if (!res.ok) throw new Error(`BTC status → ${res.status}`)
  return camel<BtcStatus>(await res.json())
}

async function fetchBtcAddress(): Promise<string> {
  if (isTauri()) {
    return tauriInvoke<string>('get_btc_address')
  }
  const res = await fetch(`${RPC_BASE}/api/v1/btc/address`)
  if (!res.ok) throw new Error(`BTC address → ${res.status}`)
  const data = await res.json()
  return data.address
}

// ─── Hooks ────────────────────────────────────────────────────────────────────

export function useBtcStatus(intervalMs = 10_000) {
  const [status, setStatus] = useState<BtcStatus | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let active = true
    const poll = async () => {
      try {
        const data = await fetchBtcStatus()
        if (active) {
          setStatus(data)
          setError(null)
        }
      } catch (e) {
        if (active) setError(e instanceof Error ? e.message : String(e))
      }
    }
    poll()
    const id = setInterval(poll, intervalMs)
    return () => {
      active = false
      clearInterval(id)
    }
  }, [intervalMs])

  return { status, error }
}

export function useBtcAddress() {
  const [address, setAddress] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const generate = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      const addr = await fetchBtcAddress()
      setAddress(addr)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setLoading(false)
    }
  }, [])

  return { address, generate, loading, error }
}

export function useSendBtc() {
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const send = useCallback(async (toAddress: string, amountSatoshis: number): Promise<string | null> => {
    setLoading(true)
    setError(null)
    try {
      if (isTauri()) {
        return await tauriInvoke<string>('send_btc', { toAddress, amountSatoshis })
      }
      const res = await fetch(`${RPC_BASE}/api/v1/btc/send`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ to_address: toAddress, amount_satoshis: amountSatoshis }),
      })
      if (!res.ok) throw new Error(`BTC send → ${res.status}`)
      const data = await res.json()
      return data.txid
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      return null
    } finally {
      setLoading(false)
    }
  }, [])

  return { send, loading, error }
}
