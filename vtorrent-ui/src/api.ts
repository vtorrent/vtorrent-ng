// Shared RPC client for browser (non-Tauri) mode.
//
// Every hook previously re-implemented RPC_BASE, Tauri detection, auth
// headers, and snake→camel normalization — with drift (useBtc bypassed the
// auth headers entirely). This module is the single source of truth.

/** Optional RPC API key for browser (non-Tauri) mode. */
export function getRpcApiKey(): string | null {
  if (isTauri()) return null
  try {
    return window.localStorage.getItem('vtorrent.rpc_api_key')
  } catch {
    return null
  }
}

export function authHeaders(): Record<string, string> {
  const key = getRpcApiKey()
  return key ? { 'X-API-Key': key } : {}
}

// ─── Tauri detection ──────────────────────────────────────────────────────────

export function isTauri(): boolean {
  return typeof (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ !== 'undefined'
}

export async function tauriInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import('@tauri-apps/api/core')
  return invoke<T>(cmd, args)
}

// ─── Generic fetch helpers ─────────────────────────────────────────────────────

export async function rpcGet<T>(path: string): Promise<T> {
  const res = await fetch(`${RPC_BASE}${path}`, { headers: authHeaders() })
  if (!res.ok) throw new Error(`RPC ${path} → ${res.status}`)
  return res.json() as Promise<T>
}

export async function rpcPost<T>(path: string, body: unknown): Promise<T> {
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

export async function rpcDelete<T>(path: string): Promise<T> {
  const res = await fetch(`${RPC_BASE}${path}`, {
    method: 'DELETE',
    headers: authHeaders(),
  })
  if (!res.ok) throw new Error(`RPC DELETE ${path} → ${res.status}`)
  return res.json() as Promise<T>
}

// ─── Snake → camel key normaliser (deep) ─────────────────────────────────────

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function camel(obj: any): any {
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

export const RPC_BASE = 'http://127.0.0.1:22525'