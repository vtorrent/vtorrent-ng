import { useCallback, useEffect, useState } from 'react'
import { Coins, Zap, Download, ArrowLeftRight, RefreshCw } from 'lucide-react'
import { formatVTR } from '../hooks/useWallet'
import { getEarningsSummary, type EarningsSummary } from '../hooks/useNode'

// ─── Helpers ──────────────────────────────────────────────────────────────────

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`
}

const WINDOWS = ['24h', '7d', '30d', 'all'] as const

// ─── Page ─────────────────────────────────────────────────────────────────────

export default function EarningsPage() {
  const [window, setWindow] = useState<(typeof WINDOWS)[number]>('30d')
  const [data, setData] = useState<EarningsSummary | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const [reloadNonce, setReloadNonce] = useState(0)
  const reload = useCallback(() => setReloadNonce(n => n + 1), [])

  useEffect(() => {
    let cancelled = false
    const load = async () => {
      setLoading(true)
      setError(null)
      try {
        const d = await getEarningsSummary(window)
        if (!cancelled) setData(d)
      } catch (e: unknown) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e))
      } finally {
        if (!cancelled) setLoading(false)
      }
    }
    void load()
    return () => {
      cancelled = true
    }
  }, [window, reloadNonce])

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold flex items-center gap-2">
            <Coins size={22} className="text-amber-400" /> Earnings
          </h1>
          <p className="text-sm text-slate-400 mt-1">
            Staking rewards, torrent seeding incentives, and swap activity.
          </p>
        </div>
        <div className="flex items-center gap-2">
          <div className="flex rounded-lg overflow-hidden border border-slate-700">
            {WINDOWS.map(w => (
              <button
                key={w}
                onClick={() => setWindow(w)}
                className={`px-3 py-1.5 text-xs ${
                  window === w ? 'bg-amber-500/20 text-amber-300' : 'text-slate-400 hover:text-slate-200'
                }`}
              >
                {w}
              </button>
            ))}
          </div>
          <button onClick={reload} className="btn-secondary text-xs" aria-label="Refresh">
            <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
          </button>
        </div>
      </div>

      {error && (
        <div className="card border-red-500/40 text-red-300 text-sm">
          Could not load earnings: {error}
        </div>
      )}

      {/* Headline total */}
      <div className="card">
        <div className="text-xs uppercase tracking-wide text-slate-500">
          Total earned ({window})
        </div>
        <div className="text-3xl font-semibold mt-1 text-amber-300">
          {data ? formatVTR(data.totalEarnedSats) : '—'}
        </div>
        {data && (
          <div className="text-xs text-slate-500 mt-1">as of block {data.asOfHeight}</div>
        )}
      </div>

      {/* Category cards */}
      <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
        <div className="card">
          <div className="flex items-center gap-2 text-slate-300">
            <Zap size={16} className="text-emerald-400" /> Staking
          </div>
          <div className="text-2xl font-semibold mt-2">
            {data ? formatVTR(data.staking.rewardsSats) : '—'}
          </div>
          <div className="text-xs text-slate-500 mt-1">
            {data ? `${data.staking.blocks} reward block(s)` : ''}
          </div>
        </div>

        <div className="card">
          <div className="flex items-center gap-2 text-slate-300">
            <Download size={16} className="text-sky-400" /> Torrents
          </div>
          <div className="text-2xl font-semibold mt-2">
            {data ? formatVTR(data.torrents.earnedSats) : '—'}
          </div>
          <div className="text-xs text-slate-500 mt-1">
            {data
              ? `${data.torrents.sessions} session(s) · paid ${formatVTR(data.torrents.paidSats)} · ↑${formatBytes(data.torrents.uploadedBytes)} ↓${formatBytes(data.torrents.downloadedBytes)}`
              : ''}
          </div>
        </div>

        <div className="card">
          <div className="flex items-center gap-2 text-slate-300">
            <ArrowLeftRight size={16} className="text-violet-400" /> Swaps
          </div>
          <div className="text-2xl font-semibold mt-2">
            {data ? `${data.swaps.completed}` : '—'}
          </div>
          <div className="text-xs text-slate-500 mt-1">
            {data ? `completed · ${data.swaps.open} open` : ''}
          </div>
        </div>
      </div>

      <p className="text-xs text-slate-500">
        Torrent earnings are net of payments to peers. Amounts are informational.
      </p>
    </div>
  )
}
