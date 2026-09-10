import { useState } from 'react'
import { camel, isTauri, rpcGet, tauriInvoke } from '../../api'
import { dailyAvgReward, type RewardPoint } from '../../utils/stakingOps'
import { formatVTR } from '../../hooks/useWallet'

interface RewardRow {
  height: number
  timestamp: number
  blockHash: string
  rewardSats: number
  stakerAddress: string | null
}

async function fetchRewards(): Promise<RewardRow[]> {
  if (isTauri()) {
    return tauriInvoke<RewardRow[]>('get_staking_rewards', { limit: 20 })
  }
  const raw = await rpcGet<unknown>('/api/v1/staking/rewards?limit=20')
  const data = camel(raw) as { tipHeight: number, rewards: RewardRow[] }
  return data.rewards
}

export default function RewardHistory({ tipHeight, blocksStaked }: { tipHeight: number | null, blocksStaked: number }) {
  const [points, setPoints] = useState<RewardPoint[]>([])
  const [rows, setRows] = useState<RewardRow[]>([])
  const [loading, setLoading] = useState(false)
  const [expanded, setExpanded] = useState(false)
  const [loadError, setLoadError] = useState<string | null>(null)

  const load = async () => {
    if (tipHeight == null || loading) return
    setLoading(true)
    setLoadError(null)
    try {
      const rewards = await fetchRewards()
      setRows(rewards)
      setPoints(rewards.map(r => ({ height: r.height, timestamp: r.timestamp, rewardSats: r.rewardSats })))
      setExpanded(true)
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e))
    } finally {
      setLoading(false)
    }
  }

  const avg = dailyAvgReward(points.filter(p => p.rewardSats > 0), Math.floor(Date.now() / 1000))

  return (
    <div className="bg-navy-900/40 border border-vtorrent-900/20 rounded-xl p-5 space-y-3">
      <h2 className="text-sm font-medium text-gray-300">Recent Rewards ({blocksStaked} staked)</h2>
      {!expanded ? (
        <button onClick={load} disabled={tipHeight == null || loading} className="text-xs text-vtorrent-400 underline disabled:opacity-50">
          {loading ? 'Loading…' : 'Show last 20 blocks'}
        </button>
      ) : (
        <>
          <p className="text-xs text-gray-400 font-mono">Daily avg: {formatVTR(Math.round(avg))} / day</p>
          <ul className="space-y-1">
            {rows.map(r => (
              <li key={r.height} className="text-xs text-gray-400 font-mono">
                {r.height} · {new Date(r.timestamp * 1000).toLocaleString()} · {formatVTR(r.rewardSats)} · {r.stakerAddress ?? '—'}
              </li>
            ))}
          </ul>
        </>
      )}
      {loadError && <p className="text-xs text-red-400" role="status">{loadError}</p>}
    </div>
  )
}
