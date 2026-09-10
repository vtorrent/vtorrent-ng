import { useNodeInfo } from '../../hooks/useNode'
import { healthSummary } from '../../utils/stakingOps'

export default function HealthStrip() {
  const { data, loading, error } = useNodeInfo(10_000)
  if (loading || !data) return <p className="text-xs text-gray-500">Loading node health…</p>
  if (error) return <p className="text-xs text-red-400">Node health unavailable: {error}</p>
  return (
    <div className="px-4 py-3 rounded-lg bg-navy-900/30 border border-vtorrent-900/20 text-xs text-gray-300 font-mono">
      {healthSummary(data)}
      <a
        className="ml-3 text-vtorrent-400 hover:text-vtorrent-300 underline"
        href="http://127.0.0.1:3300"
        target="_blank"
        rel="noreferrer"
      >
        Grafana
      </a>
    </div>
  )
}
