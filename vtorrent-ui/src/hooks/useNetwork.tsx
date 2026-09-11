import { useEffect, useState } from 'react'

export type Network = 'mainnet' | 'testnet'

const STORAGE_KEY = 'vtr-network'
const SEEDS_KEY = 'vtr-seeds'

/// Well-known local endpoints tried by seed auto-detect.
export const DEFAULT_PROBE_CANDIDATES = ['127.0.0.1:22526', '127.0.0.1:22527']

/// Split a comma-separated seed-peer input into clean `host:port` entries.
export function parseSeeds(input: string): string[] {
  return input
    .split(',')
    .map(s => s.trim())
    .filter(s => s.length > 0)
}

/// Which network the embedded node should join. Persisted; defaults to
/// mainnet so existing installs never silently change networks.
export function useNetwork() {
  const [network, setNetwork] = useState<Network>(() => {
    try {
      return localStorage.getItem(STORAGE_KEY) === 'testnet' ? 'testnet' : 'mainnet'
    } catch {
      // Private mode: fall back to mainnet for this session.
      return 'mainnet'
    }
  })

  useEffect(() => {
    try {
      localStorage.setItem(STORAGE_KEY, network)
    } catch {
      // Private mode: network still applies for this session.
    }
  }, [network])

  const [seeds, setSeeds] = useState<string>(() => {
    try {
      return localStorage.getItem(SEEDS_KEY) ?? ''
    } catch {
      return ''
    }
  })

  useEffect(() => {
    try {
      localStorage.setItem(SEEDS_KEY, seeds)
    } catch {
      // Private mode: seeds still apply for this session.
    }
  }, [seeds])

  return { network, setNetwork, seeds, setSeeds }
}
