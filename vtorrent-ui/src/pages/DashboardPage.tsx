import { useState } from 'react'
import { Copy, Plus, RefreshCw, TrendingUp, Coins, ArrowUpRight, ArrowDownLeft, Clock, Wifi, WifiOff } from 'lucide-react'
import { useWallet, formatVTR } from '../hooks/useWallet'
import { useTransactions, useNodeInfo, type TxRecord } from '../hooks/useNode'

// ─── Helpers ──────────────────────────────────────────────────────────────────

/** Convert a Unix timestamp to a human-readable relative time string. */
function relativeTime(ts: number): string {
  const diff = Math.floor(Date.now() / 1000) - ts
  if (diff < 60) return `${diff}s ago`
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`
  return `${Math.floor(diff / 86400)}d ago`
}

/** Derive a UI-friendly tx type from the Rust TxType debug string. */
function txKind(txType: string): 'receive' | 'send' | 'stake' | 'other' {
  const t = txType.toLowerCase()
  if (t.includes('stake') || t.includes('coinbase') || t.includes('reward')) return 'stake'
  if (t.includes('send') || t.includes('transfer')) return 'send'
  if (t.includes('receive') || t.includes('payment')) return 'receive'
  return 'other'
}

/** Shorten a txid for display. */
function shortTxid(txid: string): string {
  if (txid.length <= 16) return txid
  return `${txid.slice(0, 8)}…${txid.slice(-8)}`
}

// ─── Component ────────────────────────────────────────────────────────────────

export default function DashboardPage() {
  const { keys, defaultAddress, totalBalance, legacyImportCount, generateAddress } = useWallet()
  const [copied, setCopied] = useState('')
  const [generatingKey, setGeneratingKey] = useState(false)

  // Live node info (block height, connections, sync status)
  const { data: nodeInfo } = useNodeInfo(10_000)

  // Live transaction history — last 20 confirmed txs
  const { data: txs, loading: txLoading, error: txError, refresh: refreshTxs } = useTransactions(20)

  const copyAddress = (addr: string) => {
    navigator.clipboard.writeText(addr)
    setCopied(addr)
    setTimeout(() => setCopied(''), 2000)
  }

  const handleGenerateKey = async () => {
    setGeneratingKey(true)
    try {
      await generateAddress()
    } finally {
      setGeneratingKey(false)
    }
  }

  return (
    <div className="p-6 space-y-6">
      {/* Page header */}
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-bold text-white">Dashboard</h1>
          <p className="text-gray-500 text-sm mt-0.5">Your vTorrent wallet overview</p>
        </div>
        {/* Node connectivity badge */}
        {nodeInfo ? (
          <div className="flex items-center gap-1.5 text-xs text-emerald-400">
            <Wifi size={13} />
            <span>Block {nodeInfo.blockHeight.toLocaleString()} · {nodeInfo.connections} peers</span>
          </div>
        ) : (
          <div className="flex items-center gap-1.5 text-xs text-gray-600">
            <WifiOff size={13} />
            <span>Node offline</span>
          </div>
        )}
      </div>

      {/* Stats row */}
      <div className="grid grid-cols-3 gap-4">
        <div className="card">
          <p className="text-xs text-gray-500 mb-1">Total Balance</p>
          <p className="text-xl font-bold text-vtorrent-300 font-mono">{formatVTR(totalBalance)}</p>
          <p className="text-xs text-gray-600 mt-1">≈ $0.00 USD</p>
        </div>
        <div className="card">
          <p className="text-xs text-gray-500 mb-1">Addresses</p>
          <p className="text-xl font-bold text-white">{keys.length}</p>
          {legacyImportCount > 0 && (
            <p className="text-xs text-amber-400 mt-1">{legacyImportCount} legacy imported</p>
          )}
        </div>
        <div className="card">
          <p className="text-xs text-gray-500 mb-1">Staking Status</p>
          <p className="text-xl font-bold text-emerald-400">Active</p>
          <p className="text-xs text-gray-600 mt-1">~5% APR</p>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-4">
        {/* Addresses */}
        <div className="card">
          <div className="flex items-center justify-between mb-4">
            <h2 className="font-semibold text-white text-sm">Addresses</h2>
            <button
              onClick={handleGenerateKey}
              disabled={generatingKey}
              className="flex items-center gap-1.5 text-xs text-vtorrent-400 hover:text-vtorrent-300 transition-colors"
            >
              {generatingKey
                ? <RefreshCw size={12} className="animate-spin" />
                : <Plus size={12} />
              }
              New Address
            </button>
          </div>

          <div className="space-y-2">
            {keys.map(key => (
              <div
                key={key.address}
                className={`rounded-lg p-3 border transition-all ${
                  key.address === defaultAddress
                    ? 'bg-vtorrent-500/8 border-vtorrent-700/40'
                    : 'bg-navy-800/40 border-navy-700/40'
                }`}
              >
                <div className="flex items-start justify-between gap-2">
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2 mb-1">
                      {key.address === defaultAddress && (
                        <span className="text-xs text-vtorrent-400 font-medium">Default</span>
                      )}
                      {key.isLegacyImport && (
                        <span className="badge-yellow text-xs">Legacy</span>
                      )}
                    </div>
                    <p className="font-mono text-xs text-vtorrent-300 truncate">{key.address}</p>
                    {key.label && (
                      <p className="text-xs text-gray-500 mt-0.5">{key.label}</p>
                    )}
                  </div>
                  <button
                    onClick={() => copyAddress(key.address)}
                    className="text-gray-600 hover:text-vtorrent-400 transition-colors flex-shrink-0"
                  >
                    {copied === key.address
                      ? <span className="text-xs text-vtorrent-400">Copied!</span>
                      : <Copy size={12} />
                    }
                  </button>
                </div>
                <div className="mt-2 flex items-center justify-between">
                  <span className="font-mono text-xs text-gray-400">{formatVTR(key.balance)}</span>
                </div>
              </div>
            ))}
          </div>
        </div>

        {/* Recent transactions — live data */}
        <div className="card">
          <div className="flex items-center justify-between mb-4">
            <h2 className="font-semibold text-white text-sm">Recent Transactions</h2>
            <button
              onClick={refreshTxs}
              className="text-gray-600 hover:text-vtorrent-400 transition-colors"
              title="Refresh"
            >
              <RefreshCw size={12} />
            </button>
          </div>

          {txLoading && txs.length === 0 ? (
            <div className="flex items-center gap-2 text-gray-600 text-xs py-4">
              <RefreshCw size={12} className="animate-spin" />
              Loading transactions…
            </div>
          ) : txError ? (
            <div className="text-xs text-red-400 py-4">
              Could not load transactions — node may be offline.
            </div>
          ) : txs.length === 0 ? (
            <div className="flex items-center gap-2 text-gray-600 text-xs py-4">
              <Clock size={12} />
              No confirmed transactions yet.
            </div>
          ) : (
            <div className="space-y-3">
              {txs.map((tx: TxRecord) => {
                const kind = txKind(tx.txType)
                return (
                  <div key={tx.txid} className="flex items-center gap-3 py-2 border-b border-navy-800/60 last:border-0">
                    <div className={`w-8 h-8 rounded-lg flex items-center justify-center flex-shrink-0 ${
                      kind === 'receive' ? 'bg-emerald-900/30' :
                      kind === 'send'    ? 'bg-red-900/30'     :
                      kind === 'stake'   ? 'bg-vtorrent-900/30' :
                      'bg-gray-800/40'
                    }`}>
                      {kind === 'receive' ? <ArrowDownLeft size={14} className="text-emerald-400" /> :
                       kind === 'send'    ? <ArrowUpRight  size={14} className="text-red-400" />     :
                       kind === 'stake'   ? <TrendingUp    size={14} className="text-vtorrent-400" /> :
                       <Clock size={14} className="text-gray-500" />}
                    </div>
                    <div className="flex-1 min-w-0">
                      <p className="text-xs font-medium text-gray-300 capitalize">{tx.txType.toLowerCase()}</p>
                      <p className="text-xs text-gray-600 font-mono truncate">{shortTxid(tx.txid)}</p>
                    </div>
                    <div className="text-right flex-shrink-0">
                      <p className={`text-xs font-mono font-medium ${
                        kind === 'send' ? 'text-red-400' : 'text-emerald-400'
                      }`}>
                        {kind === 'send' ? '-' : '+'}{tx.display}
                      </p>
                      <p className="text-xs text-gray-600">{relativeTime(tx.timestamp)}</p>
                    </div>
                  </div>
                )
              })}
            </div>
          )}

          {/* Claim legacy VTR CTA */}
          {legacyImportCount > 0 && (
            <div className="mt-4 bg-amber-900/15 border border-amber-800/30 rounded-lg p-3">
              <div className="flex items-center gap-2 mb-1">
                <Coins size={14} className="text-amber-400" />
                <p className="text-amber-300 text-xs font-medium">Legacy VTR Ready to Claim</p>
              </div>
              <p className="text-gray-500 text-xs">
                You have {legacyImportCount} imported legacy address{legacyImportCount !== 1 ? 'es' : ''}.
                Claim your VTR once the new chain launches.
              </p>
              <button className="mt-2 text-xs text-amber-400 hover:text-amber-300 font-medium transition-colors">
                Claim Now →
              </button>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
