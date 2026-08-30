import { useState, useEffect, useCallback } from 'react'
import { camel, isTauri, rpcGet, rpcPost, tauriInvoke } from '../api'

// ─── Types ────────────────────────────────────────────────────────────────────

export interface BtcStatus {
  initialized: boolean
  balanceSatoshis: number
  address: string | null
  bestHeight: number
  synced: boolean
}

// ─── Fetch functions ──────────────────────────────────────────────────────────

async function fetchBtcStatus(): Promise<BtcStatus> {
  if (isTauri()) {
    return tauriInvoke<BtcStatus>('get_btc_status')
  }
  return camel(await rpcGet<unknown>('/api/v1/btc/status')) as BtcStatus
}

async function fetchBtcAddress(): Promise<string> {
  if (isTauri()) {
    return tauriInvoke<string>('get_btc_address')
  }
  const data = (await rpcGet<{ address: string }>('/api/v1/btc/address')) as { address: string }
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
      const data = (await rpcPost<{ txid: string }>('/api/v1/btc/send', {
        to_address: toAddress,
        amount_satoshis: amountSatoshis,
      })) as { txid: string }
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
