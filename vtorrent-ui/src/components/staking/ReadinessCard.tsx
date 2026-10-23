import { AlertCircle, CheckCircle, Clock, ShieldCheck } from 'lucide-react'
import { formatVTR } from '../../hooks/useWallet'
import type { NodeInfo, StakingStatus } from '../../hooks/useNode'

function duration(seconds: number | null | undefined): string {
  if (!seconds || seconds <= 0) return '—'
  if (seconds < 3600) return `${Math.ceil(seconds / 60)}m`
  if (seconds < 86400) return `${Math.ceil(seconds / 3600)}h`
  return `${Math.ceil(seconds / 86400)}d`
}

function CheckRow({
  label,
  state,
  detail,
}: {
  label: string
  state: 'done' | 'pending' | 'error'
  detail?: string
}) {
  const icon = state === 'done'
    ? <CheckCircle size={14} className="text-emerald-400" />
    : state === 'error'
      ? <AlertCircle size={14} className="text-red-400" />
      : <Clock size={14} className="text-amber-400" />
  return (
    <div className="flex items-center gap-2 text-xs">
      {icon}
      <span className={state === 'pending' ? 'text-gray-500' : 'text-gray-300'}>{label}</span>
      {detail && <span className="text-gray-600">· {detail}</span>}
    </div>
  )
}

interface ReadinessCardProps {
  status: StakingStatus | null
  node: NodeInfo | null
  totalBalance: number
  effectiveAddress: string
  isEnabled: boolean
}

export default function ReadinessCard({
  status,
  node,
  totalBalance,
  effectiveAddress,
  isEnabled,
}: ReadinessCardProps) {
  const walletUnlocked = status?.walletUnlocked ?? false
  const connected = Boolean(node && node.connections > 0)
  const synced = Boolean(node && !node.syncing)
  const owned = status?.owned ?? false
  const eligible = status?.eligibleStakingSatoshis ?? 0
  const immature = status?.immatureStakingSatoshis ?? 0
  const expired = status?.expiredStakingSatoshis ?? 0
  const nextMaturity = status?.nextMaturityInSeconds ?? null
  const minAge = status?.minStakeAgeSeconds ?? 6 * 60 * 60
  const maxAge = status?.maxStakeAgeSeconds ?? 6 * 24 * 60 * 60

  let title = 'Checking readiness'
  let body = 'Loading staking status…'

  if (status) {
    if (!walletUnlocked) {
      title = 'Unlock the wallet'
      body = 'The wallet must be unlocked before staking can sign coinstakes.'
    } else if (!node) {
      title = 'Start the node'
      body = 'The node is not running. Start it before staking.'
    } else if (!connected) {
      title = 'Connect to a peer'
      body = 'The node has no peers. Staking needs a connected network view.'
    } else if (node.syncing) {
      title = 'Wait for the node to sync'
      body = `The node is syncing at ${node.syncPercent.toFixed(0)}%.`
    } else if (!effectiveAddress) {
      title = 'Create or unlock a wallet'
      body = 'No staking address is available.'
    } else if (status.warning) {
      title = 'Use an address owned by the hot wallet'
      body = status.warning
    } else if (eligible === 0 && immature > 0) {
      title = 'Wait for funds to mature'
      body = `${formatVTR(immature)} matures in ${duration(nextMaturity)}.`
    } else if (eligible === 0 && expired > 0) {
      title = 'Refresh expired stake'
      body = `${formatVTR(expired)} is older than the maximum stake age. Move it to a new address to restart maturity.`
    } else if (eligible === 0) {
      title = 'Fund the staking address'
      body = 'Send at least 1 VTR to the staking address, then wait for maturity.'
    } else if (!isEnabled) {
      title = 'Ready to stake'
      body = `${formatVTR(eligible)} is eligible. Start staking when you are ready.`
    } else {
      title = 'Staking is active'
      body = `${formatVTR(eligible)} is eligible. Rewards are estimates based on current stake weight.`
    }
  }

  return (
    <div className="bg-navy-900/40 border border-vtorrent-900/20 rounded-xl p-5 space-y-4">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-medium text-gray-300 flex items-center gap-2">
            <ShieldCheck size={15} className="text-vtorrent-400" />
            Staking readiness
          </h2>
          <p className="text-sm font-medium text-white mt-2">{title}</p>
          <p className="text-xs text-gray-400 mt-0.5">{body}</p>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-x-6 gap-y-2 text-xs">
        <div className="flex justify-between gap-3">
          <span className="text-gray-500">Available balance</span>
          <span className="font-mono text-gray-300">{formatVTR(totalBalance)}</span>
        </div>
        <div className="flex justify-between gap-3">
          <span className="text-gray-500">Eligible stake</span>
          <span className="font-mono text-emerald-300">{formatVTR(eligible)}</span>
        </div>
        <div className="flex justify-between gap-3">
          <span className="text-gray-500">Immature</span>
          <span className="font-mono text-amber-300">
            {formatVTR(immature)}
            {nextMaturity ? ` · ${duration(nextMaturity)}` : ''}
          </span>
        </div>
        <div className="flex justify-between gap-3">
          <span className="text-gray-500">Expired</span>
          <span className="font-mono text-gray-400">{formatVTR(expired)}</span>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-x-6 gap-y-2">
        <CheckRow label="Wallet unlocked" state={walletUnlocked ? 'done' : 'error'} />
        <CheckRow label="Node connected" state={connected ? 'done' : 'error'} />
        <CheckRow label="Node synced" state={synced ? 'done' : 'pending'} />
        <CheckRow label="Address owned" state={owned ? 'done' : 'error'} />
        <CheckRow label="Eligible balance" state={eligible > 0 ? 'done' : 'pending'} />
        <CheckRow label="Staking active" state={isEnabled ? 'done' : 'pending'} />
      </div>

      <p className="text-xs text-gray-600">
        Maturity window: {duration(minAge)} to {duration(maxAge)}. Expected rewards are
        estimates; actual rewards vary with network stake and luck.
      </p>
    </div>
  )
}
