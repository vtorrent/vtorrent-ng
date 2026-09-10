import { useState } from 'react'
import { camel, isTauri, rpcGet } from '../../api'
import { dailyAvgReward, type RewardPoint } from '../../utils/stakingOps'
import { formatVTR } from '../../hooks/useWallet'

async function fetchBlock(height: number): Promise<{ timestamp: number }> {
  if (isTauri()) {
    throw new Error('Reward history needs RPC web mode in v1 (no Tauri get_block_by_height command).')
  }
  const raw = await rpcGet<unknown>(`/api/v1/blockchain/block/height/${height}`)
  return camel(raw) as { timestamp: number }
}

export default function RewardHistory({ tipHeight, blocksStaked }: { tipHeight: number | null, blocksStaked: number }) {
  const [points, setPoints] = useState<RewardPoint[]>([])
  const [loading, setLoading] = useState(false)
  const [expanded, setExpanded] = useState(false)
  const [loadError, setLoadError] = useState<string | null>(null)

  const load = async () => {
    if (tipHeight == null || loading) return
    setLoading(true)
    setLoadError(null)
    try {
      const out: RewardPoint[] = []
      for (let h = tipHeight; h > tipHeight - 20 && h > 0; h--) {
        const b = await fetchBlock(h)
        out.push({ height: h, timestamp: b.timestamp, rewardSats: 0 })
      }
      setPoints(out)
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
        <p className="text-xs text-gray-400 font-mono">Historical rewards (v2 endpoint pending): {formatVTR(Math.round(avg))} / day</p>
      )}
      {loadError && <p className="text-xs text-red-400" role="status">{loadError}</p>}
    </div>
  )
}
