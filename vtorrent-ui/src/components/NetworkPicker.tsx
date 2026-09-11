import { useState } from 'react'
import type { Network } from '../hooks/useNetwork'
import { FlaskConical, ChevronDown } from 'lucide-react'

interface NetworkPickerProps {
  network: Network
  onChange: (network: Network) => void
  seeds: string
  onSeedsChange: (seeds: string) => void
}

/// Mainnet/testnet selector with an optional seed-peer field for testnet.
/// The embedded node joins the chosen network when it starts at unlock.
export default function NetworkPicker({ network, onChange, seeds, onSeedsChange }: NetworkPickerProps) {
  const [expanded, setExpanded] = useState(false)
  const showSeeds = network === 'testnet' && (expanded || seeds.length > 0)
  return (
    <div className="space-y-2.5">
      <div className="grid grid-cols-2 gap-2 p-1 rounded-lg bg-navy-900/60 border border-vtorrent-900/30">
        {(['mainnet', 'testnet'] as Network[]).map(n => (
          <button
            key={n}
            type="button"
            onClick={() => onChange(n)}
            className={`px-3 py-1.5 rounded-md text-xs font-medium capitalize transition-all duration-150 ${
              network === n
                ? 'bg-vtorrent-500/20 text-vtorrent-300 border border-vtorrent-500/40'
                : 'text-gray-500 hover:text-gray-300 border border-transparent'
            }`}
          >
            {n}
          </button>
        ))}
      </div>
      {network === 'testnet' && (
        <p className="text-[11px] text-gray-500">
          Testnet joins the soak network (regtest chain — coins are worthless).
        </p>
      )}
      {network === 'testnet' && (
        <div>
          <button
            type="button"
            onClick={() => setExpanded(e => !e)}
            className="flex items-center gap-1.5 text-xs text-gray-500 hover:text-gray-300 transition-colors"
          >
            <FlaskConical size={12} className="text-amber-400" />
            Custom seed peers
            <span className="text-gray-600 font-normal">(optional)</span>
            <ChevronDown
              size={12}
              className={`transition-transform duration-150 ${showSeeds ? 'rotate-180' : ''}`}
            />
          </button>
          {showSeeds && (
            <input
              type="text"
              className="input-field font-mono mt-2"
              placeholder="127.0.0.1:22526"
              value={seeds}
              onChange={e => onSeedsChange(e.target.value)}
            />
          )}
        </div>
      )}
    </div>
  )
}
