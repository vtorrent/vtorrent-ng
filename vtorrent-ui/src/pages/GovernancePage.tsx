import { useEffect, useState } from 'react'
import { Landmark, RefreshCw } from 'lucide-react'
import { formatVTR } from '../hooks/useWallet'
import {
  getGovernanceParams,
  getGovernanceProposals,
  type GovernanceParams,
  type GovernanceProposal,
} from '../hooks/useNode'

export default function GovernancePage() {
  const [params, setParams] = useState<GovernanceParams | null>(null)
  const [proposals, setProposals] = useState<GovernanceProposal[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const [reloadNonce, setReloadNonce] = useState(0)
  const load = () => setReloadNonce(n => n + 1)

  useEffect(() => {
    let cancelled = false
    const run = async () => {
      setLoading(true)
      setError(null)
      try {
        const [p, pr] = await Promise.all([getGovernanceParams(), getGovernanceProposals()])
        if (!cancelled) {
          setParams(p)
          setProposals(pr.proposals)
        }
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e))
      } finally {
        if (!cancelled) setLoading(false)
      }
    }
    void run()
    return () => {
      cancelled = true
    }
  }, [reloadNonce])

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold flex items-center gap-2">
          <Landmark size={22} className="text-amber-400" /> Governance
        </h1>
        <button onClick={load} className="btn-secondary text-xs" aria-label="Refresh">
          <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
        </button>
      </div>

      {error && <div className="card border-red-500/40 text-red-300 text-sm">{error}</div>}

      <div className="card">
        <h2 className="font-semibold text-white text-sm mb-3">Consensus parameters</h2>
        {params ? (
          <div className="grid grid-cols-2 md:grid-cols-3 gap-3 text-sm">
            <Field label="Annual rate" value={`${(params.posAnnualRateBps / 100).toFixed(2)}%`} />
            <Field label="Target block time" value={`${params.targetBlockTime}s`} />
            <Field label="Min stake" value={formatVTR(params.minStakeAmount)} />
            <Field label="Min stake age" value={`${Math.round(params.minStakeAge / 3600)}h`} />
            <Field label="Max stake age" value={`${Math.round(params.maxStakeAge / 86400)}d`} />
            <Field label="Reward age cap" value={`${Math.round(params.rewardAgeCap / 86400)}d`} />
          </div>
        ) : (
          <p className="text-sm text-slate-500">Loading…</p>
        )}
      </div>

      <div className="card">
        <h2 className="font-semibold text-white text-sm mb-3">Proposals</h2>
        {proposals.length === 0 ? (
          <p className="text-sm text-slate-500">No proposals.</p>
        ) : (
          <div className="space-y-2">
            {proposals.map(p => (
              <div key={p.id} className="border border-slate-700 rounded-lg p-3 text-sm">
                <div className="flex items-center justify-between">
                  <span className="font-medium">
                    {p.param} → {p.newValue}
                  </span>
                  <span
                    className={
                      p.decided ? (p.passed ? 'text-emerald-400' : 'text-red-400') : 'text-amber-400'
                    }
                  >
                    {p.decided ? (p.passed ? 'Passed' : 'Failed') : 'Open'}
                  </span>
                </div>
                <div className="text-xs text-slate-400 mt-1">
                  yes {formatVTR(p.yes)} · no {formatVTR(p.no)} · abstain {formatVTR(p.abstain)}
                </div>
                <div className="text-xs text-slate-500 mt-1">
                  created {p.createdHeight} · voting ends {p.votingEnd} · activates {p.activationHeight}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  )
}

function Field({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="text-xs text-slate-500">{label}</div>
      <div className="font-mono">{value}</div>
    </div>
  )
}
