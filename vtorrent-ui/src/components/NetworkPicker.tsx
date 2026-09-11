import { useState } from 'react'
import type { Network } from '../hooks/useNetwork'
import { DEFAULT_PROBE_CANDIDATES } from '../hooks/useNetwork'
import { isTauri, tauriInvoke } from '../api'
import { FlaskConical, ChevronDown, Radar } from 'lucide-react'

interface NetworkPickerProps {
  network: Network
  onChange: (network: Network) => void
  seeds: string
  onSeedsChange: (seeds: string) => void
}

/// Mainnet/testnet selector with optional seed peers. Seeds persist across
/// sessions; the detect button probes well-known local endpoints and fills
/// in whichever peers answer.
export default function NetworkPicker({ network, onChange, seeds, onSeedsChange }: NetworkPickerProps) {
  const [expanded, setExpanded] = useState(false)
  const [scanning, setScanning] = useState(false)
  const [scanMsg, setScanMsg] = useState('')
  const showSeeds = network === 'testnet' && (expanded || seeds.length > 0)

  const detectLocal = async () => {
    setScanning(true)
    setScanMsg('')
    try {
      const found = isTauri()
        ? await tauriInvoke<string[]>('probe_seed_peers', { candidates: DEFAULT_PROBE_CANDIDATES })
        : []
      if (found.length > 0) {
        onSeedsChange(found.join(', '))
        setScanMsg(`Found ${found.length} local peer${found.length !== 1 ? 's' : ''}`)
      } else {
        setScanMsg(isTauri() ? 'No local peers answering' : 'Detection needs the desktop app')
      }
    } catch {
      setScanMsg('Detection failed')
    } finally {
      setScanning(false)
    }
  }

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
          <div className="flex items-center justify-between">
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
              <button
                type="button"
                onClick={detectLocal}
                disabled={scanning}
                className="flex items-center gap-1 text-xs text-vtorrent-400 hover:text-vtorrent-300 disabled:opacity-50 transition-colors"
              >
                <Radar size={12} className={scanning ? 'animate-spin' : ''} />
                {scanning ? 'Scanning…' : 'Detect local'}
              </button>
            )}
          </div>
          {showSeeds && (
            <input
              type="text"
              className="input-field font-mono mt-2"
              placeholder="127.0.0.1:22526"
              value={seeds}
              onChange={e => onSeedsChange(e.target.value)}
            />
          )}
          {scanMsg && (
            <p className="text-[11px] text-gray-500 mt-1.5">{scanMsg}</p>
          )}
        </div>
      )}
    </div>
  )
}
