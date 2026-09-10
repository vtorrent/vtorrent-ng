import type { NodeInfo } from '../../hooks/useNode'
import { healthSummary } from '../../utils/stakingOps'

// Testnet compose maps grafana 3000->3300 on the host; override via VITE_GRAFANA_URL outside localhost.
const GRAFANA_URL = import.meta.env.VITE_GRAFANA_URL ?? 'http://127.0.0.1:3300'

export interface NodeHealth {
  data: NodeInfo | null
  loading: boolean
  error: string | null
}

export default function HealthStrip({ info }: { info: NodeHealth }) {
  const { data, error } = info
  if (!data) {
    if (error) return <p className="text-xs text-red-400" role="status">Node health unavailable: {error}</p>
    return <p className="text-xs text-gray-500" role="status">Loading node health…</p>
  }
  return (
    <div className="px-4 py-3 rounded-lg bg-navy-900/30 border border-vtorrent-900/20 text-xs text-gray-300 font-mono" role="status">
      {healthSummary(data)}
      {error && <span className="ml-2 text-amber-400">(refresh failed)</span>}
      <a
        className="ml-3 text-vtorrent-400 hover:text-vtorrent-300 underline"
        href={GRAFANA_URL}
        target="_blank"
        rel="noreferrer noopener"
        aria-label="Open Grafana dashboard"
      >
        Grafana
      </a>
    </div>
  )
}
