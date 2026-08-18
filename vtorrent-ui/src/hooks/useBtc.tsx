import { useState, useEffect, useCallback } from 'react'

const RPC_BASE = 'http://127.0.0.1:22525'

export interface BtcStatus {
  initialized: boolean
  balanceSatoshis: number
  address: string | null
  bestHeight: number
  synced: boolean
}

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

export function useBtcStatus(intervalMs = 10_000) {
  const [status, setStatus] = useState<BtcStatus | null>(null)

  useEffect(() => {
    let active = true
    const fetchStatus = async () => {
      try {
        const res = await fetch(`${RPC_BASE}/api/v1/btc/status`)
        if (!res.ok) return
        const data = await res.json()
        if (active) setStatus(camel<BtcStatus>(data))
      } catch {
        /* ignore */
      }
    }
    fetchStatus()
    const id = setInterval(fetchStatus, intervalMs)
    return () => {
      active = false
      clearInterval(id)
    }
  }, [intervalMs])

  return status
}

export function useBtcAddress() {
  const [address, setAddress] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)

  const generate = useCallback(async () => {
    setLoading(true)
    try {
      const res = await fetch(`${RPC_BASE}/api/v1/btc/address`)
      if (!res.ok) return
      const data = await res.json()
      setAddress(data.address)
    } finally {
      setLoading(false)
    }
  }, [])

  return { address, generate, loading }
}
