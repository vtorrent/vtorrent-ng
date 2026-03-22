import { useState } from 'react'
import {
  Zap, ZapOff, RefreshCw, TrendingUp, Coins,
  CheckCircle, AlertCircle, Info, Clock,
} from 'lucide-react'
import { formatVTR, useWallet } from '../hooks/useWallet'
import { useStakingStatus, startStaking, stopStaking } from '../hooks/useNode'

// ─── Helpers ──────────────────────────────────────────────────────────────────

/** Format a Unix timestamp as a relative "last staked" string. */
function lastStakedAgo(ts: number | null | undefined): string {
  if (!ts) return 'Never'
  const diff = Math.floor(Date.now() / 1000) - ts
  if (diff < 60) return `${diff}s ago`
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`
  return `${Math.floor(diff / 86400)}d ago`
}

/** Format expected daily reward as a human-readable VTR string. */
function formatReward(satoshisPerDay: number): string {
  if (satoshisPerDay <= 0) return '0 VTR/day'
  return `${formatVTR(satoshisPerDay)} / day`
}

// ─── Component ────────────────────────────────────────────────────────────────

type ActionStatus = 'idle' | 'loading' | 'success' | 'error'

export default function StakingPage() {
  const { keys } = useWallet()
  const { data: status, loading, error, refresh } = useStakingStatus(8_000)

  const [actionStatus, setActionStatus] = useState<ActionStatus>('idle')
  const [actionMsg, setActionMsg] = useState('')
  const [selectedAddress, setSelectedAddress] = useState('')

  const isEnabled = status?.enabled ?? false
  const stakingAddress = status?.stakingAddress ?? null
  const eligibleUtxos = status?.eligibleUtxos ?? 0
  const totalStakingSats = status?.totalStakingSatoshis ?? 0
  const expectedReward = status?.expectedRewardPerDay ?? 0
  const lastStakeTime = status?.lastStakeTime ?? null
  const blocksStaked = status?.blocksStaked ?? 0

  // Default to first key if none selected
  const addressOptions = keys.map(k => k.address)
  const effectiveAddress = selectedAddress || addressOptions[0] || ''

  const handleStart = async () => {
    if (!effectiveAddress) {
      setActionMsg('No address available. Please unlock your wallet first.')
      setActionStatus('error')
      return
    }
    setActionStatus('loading')
    setActionMsg('')
    try {
      await startStaking(effectiveAddress)
      setActionStatus('success')
      setActionMsg(`Staking started on ${effectiveAddress.slice(0, 12)}…`)
      refresh()
    } catch (e) {
      setActionStatus('error')
      setActionMsg(e instanceof Error ? e.message : String(e))
    }
  }

  const handleStop = async () => {
    setActionStatus('loading')
    setActionMsg('')
    try {
      await stopStaking()
      setActionStatus('success')
      setActionMsg('Staking stopped.')
      refresh()
    } catch (e) {
      setActionStatus('error')
      setActionMsg(e instanceof Error ? e.message : String(e))
    }
  }

  return (
    <div className="p-6 max-w-3xl mx-auto space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold text-white">Staking</h1>
          <p className="text-sm text-gray-400 mt-0.5">
            Earn VTR rewards by participating in Proof-of-Stake consensus.
          </p>
        </div>
        <button
          onClick={refresh}
          disabled={loading}
          className="p-2 rounded-lg text-gray-400 hover:text-gray-200 hover:bg-navy-800/60 transition-all"
          title="Refresh"
        >
          <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />
        </button>
      </div>

      {/* Status banner */}
      {error && (
        <div className="flex items-center gap-2 px-4 py-3 rounded-lg bg-red-900/20 border border-red-800/30 text-red-400 text-sm">
          <AlertCircle size={14} />
          <span>Failed to load staking status: {error}</span>
        </div>
      )}

      {/* Stats grid */}
      <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
        <StatCard
          label="Status"
          value={isEnabled ? 'Active' : 'Inactive'}
          icon={isEnabled ? <Zap size={16} className="text-emerald-400" /> : <ZapOff size={16} className="text-gray-500" />}
          accent={isEnabled ? 'emerald' : 'gray'}
        />
        <StatCard
          label="Staking Balance"
          value={formatVTR(totalStakingSats)}
          icon={<Coins size={16} className="text-vtorrent-400" />}
          accent="vtorrent"
        />
        <StatCard
          label="Expected Reward"
          value={formatReward(expectedReward)}
          icon={<TrendingUp size={16} className="text-amber-400" />}
          accent="amber"
        />
        <StatCard
          label="Blocks Staked"
          value={blocksStaked.toLocaleString()}
          icon={<CheckCircle size={16} className="text-blue-400" />}
          accent="blue"
        />
      </div>

      {/* Details card */}
      <div className="bg-navy-900/40 border border-vtorrent-900/20 rounded-xl p-5 space-y-4">
        <h2 className="text-sm font-medium text-gray-300">Staking Details</h2>

        <div className="grid grid-cols-2 gap-x-8 gap-y-3 text-sm">
          <DetailRow label="Staking Address" value={stakingAddress ? `${stakingAddress.slice(0, 16)}…` : '—'} />
          <DetailRow label="Eligible UTXOs" value={eligibleUtxos.toString()} />
          <DetailRow label="Last Stake" value={lastStakedAgo(lastStakeTime)} />
          <DetailRow
            label="Maturity Requirement"
            value="100 confirmations"
            hint="UTXOs must be 100 blocks old to be eligible."
          />
        </div>
      </div>

      {/* Address selector (only shown when not staking) */}
      {!isEnabled && addressOptions.length > 1 && (
        <div className="space-y-2">
          <label className="text-xs font-medium text-gray-400">Staking Address</label>
          <select
            value={selectedAddress || addressOptions[0]}
            onChange={e => setSelectedAddress(e.target.value)}
            className="w-full bg-navy-900/60 border border-vtorrent-900/30 rounded-lg px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-vtorrent-500/50"
          >
            {addressOptions.map(addr => (
              <option key={addr} value={addr}>{addr}</option>
            ))}
          </select>
        </div>
      )}

      {/* Action feedback */}
      {actionStatus === 'success' && (
        <div className="flex items-center gap-2 px-4 py-3 rounded-lg bg-emerald-900/20 border border-emerald-800/30 text-emerald-400 text-sm">
          <CheckCircle size={14} />
          <span>{actionMsg}</span>
        </div>
      )}
      {actionStatus === 'error' && (
        <div className="flex items-center gap-2 px-4 py-3 rounded-lg bg-red-900/20 border border-red-800/30 text-red-400 text-sm">
          <AlertCircle size={14} />
          <span>{actionMsg}</span>
        </div>
      )}

      {/* Start / Stop button */}
      <div className="flex gap-3">
        {!isEnabled ? (
          <button
            onClick={handleStart}
            disabled={actionStatus === 'loading' || !effectiveAddress}
            className="flex items-center gap-2 px-5 py-2.5 rounded-lg bg-vtorrent-600 hover:bg-vtorrent-500 disabled:opacity-50 disabled:cursor-not-allowed text-white text-sm font-medium transition-all"
          >
            {actionStatus === 'loading' ? (
              <RefreshCw size={14} className="animate-spin" />
            ) : (
              <Zap size={14} />
            )}
            Start Staking
          </button>
        ) : (
          <button
            onClick={handleStop}
            disabled={actionStatus === 'loading'}
            className="flex items-center gap-2 px-5 py-2.5 rounded-lg bg-red-700/80 hover:bg-red-600/80 disabled:opacity-50 disabled:cursor-not-allowed text-white text-sm font-medium transition-all"
          >
            {actionStatus === 'loading' ? (
              <RefreshCw size={14} className="animate-spin" />
            ) : (
              <ZapOff size={14} />
            )}
            Stop Staking
          </button>
        )}
      </div>

      {/* Info note */}
      <div className="flex items-start gap-2 px-4 py-3 rounded-lg bg-navy-900/30 border border-vtorrent-900/20 text-gray-500 text-xs">
        <Info size={12} className="mt-0.5 shrink-0" />
        <span>
          vTorrent uses Proof-of-Stake consensus. Staking requires a minimum balance of
          1,000 VTR in a single UTXO that is at least 100 blocks old. Rewards are
          proportional to your staking weight relative to the total network stake.
        </span>
      </div>
    </div>
  )
}

// ─── Sub-components ───────────────────────────────────────────────────────────

interface StatCardProps {
  label: string
  value: string
  icon: React.ReactNode
  accent: 'emerald' | 'gray' | 'vtorrent' | 'amber' | 'blue'
}

const accentBg: Record<StatCardProps['accent'], string> = {
  emerald: 'bg-emerald-900/20 border-emerald-800/20',
  gray:    'bg-navy-900/40 border-vtorrent-900/20',
  vtorrent:'bg-vtorrent-900/20 border-vtorrent-800/20',
  amber:   'bg-amber-900/20 border-amber-800/20',
  blue:    'bg-blue-900/20 border-blue-800/20',
}

function StatCard({ label, value, icon, accent }: StatCardProps) {
  return (
    <div className={`rounded-xl border p-4 space-y-2 ${accentBg[accent]}`}>
      <div className="flex items-center gap-2">
        {icon}
        <span className="text-xs text-gray-500">{label}</span>
      </div>
      <p className="text-sm font-medium text-gray-200 font-mono truncate">{value}</p>
    </div>
  )
}

interface DetailRowProps {
  label: string
  value: string
  hint?: string
}

function DetailRow({ label, value, hint }: DetailRowProps) {
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-gray-500 text-xs flex items-center gap-1">
        {label}
        {hint && (
          <span title={hint} className="cursor-help">
            <Info size={10} />
          </span>
        )}
      </span>
      <span className="text-gray-200 font-mono text-xs">{value}</span>
    </div>
  )
}
