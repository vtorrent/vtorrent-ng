import { useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import {
  ArrowDownLeft,
  ArrowUpRight,
  CheckCircle,
  Clock,
  Copy,
  RefreshCw,
  Search,
  TrendingUp,
  X,
} from 'lucide-react'
import { formatVTR } from '../hooks/useWallet'
import { useActivity, type ActivityItem } from '../hooks/useNode'

type StatusFilter = 'all' | 'broadcast' | 'pending' | 'confirmed' | 'reorged' | 'failed'

function txKind(txType: string): 'receive' | 'send' | 'stake' | 'other' {
  const value = txType.toLowerCase()
  if (value.includes('receive') || value.includes('coinbase')) return 'receive'
  if (value.includes('send')) return 'send'
  if (value.includes('stake')) return 'stake'
  return 'other'
}

function relativeTime(ts: number): string {
  const diff = Math.floor(Date.now() / 1000) - ts
  if (diff < 60) return `${Math.max(0, diff)}s ago`
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`
  return `${Math.floor(diff / 86400)}d ago`
}

function KindIcon({ kind }: { kind: ReturnType<typeof txKind> }) {
  if (kind === 'receive') return <ArrowDownLeft size={15} className="text-emerald-400" />
  if (kind === 'send') return <ArrowUpRight size={15} className="text-red-400" />
  if (kind === 'stake') return <TrendingUp size={15} className="text-vtorrent-400" />
  return <Clock size={15} className="text-gray-500" />
}

function StatusBadge({ status }: { status: ActivityItem['status'] }) {
  if (status === 'confirmed') return <span className="badge-green">Confirmed</span>
  if (status === 'pending') return <span className="badge-yellow">Pending</span>
  if (status === 'broadcast') {
    return <span className="text-xs px-2 py-0.5 rounded-full bg-blue-900/40 text-blue-300">Broadcast</span>
  }
  if (status === 'reorged') {
    return <span className="text-xs px-2 py-0.5 rounded-full bg-amber-900/40 text-amber-300">Reorged</span>
  }
  return <span className="text-xs px-2 py-0.5 rounded-full bg-red-900/40 text-red-300">Failed</span>
}

export default function ActivityPage() {
  const [searchParams] = useSearchParams()
  const { data: activity, loading, error, refresh } = useActivity(200, 15_000)
  const [query, setQuery] = useState('')
  const [statusFilter, setStatusFilter] = useState<StatusFilter>('all')
  const [selectedTxid, setSelectedTxid] = useState<string | null>(
    searchParams.get('txid')
  )
  const [copied, setCopied] = useState(false)

  const selected = selectedTxid
    ? activity.find(item => item.txid === selectedTxid) ?? null
    : null
  const missingSelected = Boolean(selectedTxid && !selected)
  const needle = query.trim().toLowerCase()
  const filtered = activity.filter(item => {
    if (statusFilter !== 'all' && item.status !== statusFilter) return false
    if (!needle) return true
    return item.txid.toLowerCase().includes(needle)
      || item.txType.toLowerCase().includes(needle)
  })

  const copyTxid = async () => {
    if (!selected) return
    try {
      await navigator.clipboard.writeText(selected.txid)
      setCopied(true)
      window.setTimeout(() => setCopied(false), 1500)
    } catch {
      /* clipboard unavailable */
    }
  }

  return (
    <div className="p-6 max-w-4xl mx-auto space-y-5">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold text-white">Activity</h1>
          <p className="text-sm text-gray-400 mt-0.5">
            Confirmed and pending wallet transactions in one place.
          </p>
        </div>
        <button
          onClick={refresh}
          disabled={loading}
          className="p-2 rounded-lg text-gray-400 hover:text-gray-200 hover:bg-navy-800/60 transition-all"
          title="Refresh activity"
          aria-label="Refresh activity"
        >
          <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />
        </button>
      </div>

      <div className="flex flex-col sm:flex-row gap-3">
        <div className="relative flex-1">
          <Search size={14} className="absolute left-3 top-1/2 -translate-y-1/2 text-gray-600" />
          <input
            className="input-field pl-9"
            placeholder="Search by txid or type"
            value={query}
            onChange={event => setQuery(event.target.value)}
          />
        </div>
        <div className="flex gap-1 bg-navy-900/60 rounded-xl p-1">
          {(['all', 'broadcast', 'pending', 'confirmed', 'reorged', 'failed'] as const).map(status => (
            <button
              key={status}
              onClick={() => setStatusFilter(status)}
              className={`px-3 py-1.5 rounded-lg text-xs font-medium capitalize transition-all ${
                statusFilter === status
                  ? 'bg-vtorrent-500/15 text-vtorrent-300 border border-vtorrent-500/20'
                  : 'text-gray-500 hover:text-gray-300'
              }`}
            >
              {status}
            </button>
          ))}
        </div>
      </div>

      {error && (
        <div className="text-xs text-red-400 card">
          Could not load activity: {error}
        </div>
      )}

      {missingSelected && (
        <div className="card flex items-center justify-between gap-3 text-xs text-gray-400">
          <span>
            Transaction <span className="font-mono">{selectedTxid}</span> is not in the
            recent activity window.
          </span>
          <button
            onClick={() => setSelectedTxid(null)}
            className="text-vtorrent-400 hover:text-vtorrent-300"
          >
            Dismiss
          </button>
        </div>
      )}

      {loading && activity.length === 0 ? (
        <div className="card flex items-center gap-2 text-xs text-gray-500">
          <RefreshCw size={12} className="animate-spin" /> Loading activity…
        </div>
      ) : filtered.length === 0 ? (
        <div className="card text-sm text-gray-500">
          No transactions match this filter.
        </div>
      ) : (
        <div className="card divide-y divide-navy-800/60">
          {filtered.map(item => {
            const kind = txKind(item.txType)
            return (
              <button
                key={item.txid}
                type="button"
                onClick={() => setSelectedTxid(item.txid)}
                className="w-full flex items-center gap-3 py-3 text-left hover:bg-navy-800/30 transition-colors first:pt-0 last:pb-0"
              >
                <div className={`w-8 h-8 rounded-lg flex items-center justify-center flex-shrink-0 ${
                  kind === 'receive' ? 'bg-emerald-900/30' :
                  kind === 'send' ? 'bg-red-900/30' :
                  kind === 'stake' ? 'bg-vtorrent-900/30' : 'bg-gray-800/40'
                }`}>
                  <KindIcon kind={kind} />
                </div>
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <p className="text-xs font-medium text-gray-300 capitalize">{item.txType}</p>
                    <StatusBadge status={item.status} />
                  </div>
                  <p className="text-xs text-gray-600 font-mono truncate">{item.txid}</p>
                </div>
                <div className="text-right flex-shrink-0">
                  <p className={`text-xs font-mono font-medium ${
                    kind === 'send' ? 'text-red-400' : 'text-emerald-400'
                  }`}>
                    {kind === 'send' ? '-' : '+'}{formatVTR(item.amountSatoshis)}
                  </p>
                  <p className="text-xs text-gray-600">
                    {item.status === 'pending'
                      ? 'pending'
                      : `${item.confirmations} conf · ${relativeTime(item.timestamp)}`}
                  </p>
                </div>
              </button>
            )
          })}
        </div>
      )}

      {selected && (
        <div
          className="fixed inset-0 z-50 flex justify-end bg-black/50"
          onClick={() => setSelectedTxid(null)}
        >
          <div
            className="h-full w-full max-w-md bg-navy-900 border-l border-vtorrent-900/30 p-5 space-y-5 overflow-y-auto"
            onClick={event => event.stopPropagation()}
          >
            <div className="flex items-center justify-between">
              <h2 className="text-sm font-semibold text-white">Transaction details</h2>
              <button
                onClick={() => setSelectedTxid(null)}
                className="p-1.5 rounded-lg text-gray-500 hover:text-gray-300 hover:bg-navy-800"
                aria-label="Close details"
              >
                <X size={16} />
              </button>
            </div>

            <div className="space-y-3 text-sm">
              <div>
                <p className="text-xs text-gray-500 mb-1">Transaction ID</p>
                <div className="flex items-start gap-2">
                  <p className="font-mono text-xs text-gray-300 break-all flex-1">{selected.txid}</p>
                  <button
                    onClick={copyTxid}
                    className="p-1.5 rounded-lg text-gray-500 hover:text-vtorrent-300 hover:bg-navy-800"
                    title="Copy transaction ID"
                  >
                    {copied ? <CheckCircle size={14} className="text-emerald-400" /> : <Copy size={14} />}
                  </button>
                </div>
              </div>
              <div className="flex justify-between gap-4">
                <span className="text-gray-500">Status</span>
                <StatusBadge status={selected.status} />
              </div>
              <div className="flex justify-between gap-4">
                <span className="text-gray-500">Type</span>
                <span className="text-gray-300 capitalize">{selected.txType}</span>
              </div>
              <div className="flex justify-between gap-4">
                <span className="text-gray-500">Amount</span>
                <span className="font-mono text-gray-200">{formatVTR(selected.amountSatoshis)}</span>
              </div>
              <div className="flex justify-between gap-4">
                <span className="text-gray-500">Fee</span>
                <span className="font-mono text-gray-400">{formatVTR(selected.feeSatoshis)}</span>
              </div>
              <div className="flex justify-between gap-4">
                <span className="text-gray-500">Block</span>
                <span className="text-gray-400">
                  {selected.blockHeight > 0 ? selected.blockHeight.toLocaleString() : 'Pending'}
                </span>
              </div>
              <div className="flex justify-between gap-4">
                <span className="text-gray-500">Confirmations</span>
                <span className="text-gray-400">{selected.confirmations}</span>
              </div>
              <div className="flex justify-between gap-4">
                <span className="text-gray-500">Time</span>
                <span className="text-gray-400">
                  {new Date(selected.timestamp * 1000).toLocaleString()}
                </span>
              </div>
            </div>

            {selected.error && (
              <p className="text-xs text-red-300 bg-red-900/15 border border-red-800/30 rounded-lg px-3 py-2">
                {selected.error}
              </p>
            )}

            {selected.status === 'pending' && (
              <p className="text-xs text-amber-300 bg-amber-900/15 border border-amber-800/30 rounded-lg px-3 py-2">
                This transaction is in the mempool. It can still be replaced or dropped
                before it confirms.
              </p>
            )}
          </div>
        </div>
      )}
    </div>
  )
}
