import { useState } from 'react'
import {
  Upload, Download, Play, Pause, Trash2, Plus,
  Coins, TrendingUp, HardDrive, Users
} from 'lucide-react'
import { formatVTR } from '../hooks/useWallet'

interface TorrentItem {
  id: string
  name: string
  size: number
  progress: number
  status: 'downloading' | 'seeding' | 'paused' | 'complete'
  peers: number
  seeders: number
  downloadSpeed: number
  uploadSpeed: number
  earned: number
  infoHash: string
}

const mockTorrents: TorrentItem[] = [
  {
    id: '1',
    name: 'Ubuntu 24.04 LTS Desktop AMD64.iso',
    size: 5_368_709_120,
    progress: 100,
    status: 'seeding',
    peers: 14,
    seeders: 14,
    downloadSpeed: 0,
    uploadSpeed: 2_400_000,
    earned: 45_000_000,
    infoHash: 'a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2',
  },
  {
    id: '2',
    name: 'Debian 12.4 Netinstall AMD64.iso',
    size: 659_554_304,
    progress: 67,
    status: 'downloading',
    peers: 8,
    seeders: 22,
    downloadSpeed: 3_100_000,
    uploadSpeed: 450_000,
    earned: 0,
    infoHash: 'b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3',
  },
]

function formatSize(bytes: number): string {
  if (bytes >= 1_073_741_824) return (bytes / 1_073_741_824).toFixed(2) + ' GB'
  if (bytes >= 1_048_576)     return (bytes / 1_048_576).toFixed(1) + ' MB'
  return (bytes / 1024).toFixed(0) + ' KB'
}

function formatSpeed(bps: number): string {
  if (bps >= 1_048_576) return (bps / 1_048_576).toFixed(1) + ' MB/s'
  if (bps >= 1024)      return (bps / 1024).toFixed(0) + ' KB/s'
  return bps + ' B/s'
}

export default function TorrentPage() {
  const [torrents, setTorrents] = useState<TorrentItem[]>(mockTorrents)
  const [showAdd, setShowAdd] = useState(false)
  const [magnetLink, setMagnetLink] = useState('')

  const totalEarned = torrents.reduce((sum, t) => sum + t.earned, 0)
  const totalUpload = torrents.reduce((sum, t) => sum + t.uploadSpeed, 0)
  const activeSeeders = torrents.filter(t => t.status === 'seeding').length

  return (
    <div className="p-6 space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-bold text-white">Torrents</h1>
          <p className="text-gray-500 text-sm mt-0.5">Earn VTR by seeding. Pay VTR for faster downloads.</p>
        </div>
        <button
          onClick={() => setShowAdd(!showAdd)}
          className="btn-primary flex items-center gap-2"
        >
          <Plus size={16} />
          Add Torrent
        </button>
      </div>

      {/* Stats */}
      <div className="grid grid-cols-4 gap-4">
        <div className="card">
          <div className="flex items-center gap-2 mb-1">
            <Coins size={14} className="text-vtorrent-400" />
            <p className="text-xs text-gray-500">Total Earned</p>
          </div>
          <p className="font-mono font-bold text-vtorrent-300">{formatVTR(totalEarned)}</p>
        </div>
        <div className="card">
          <div className="flex items-center gap-2 mb-1">
            <TrendingUp size={14} className="text-emerald-400" />
            <p className="text-xs text-gray-500">Upload Speed</p>
          </div>
          <p className="font-mono font-bold text-emerald-300">{formatSpeed(totalUpload)}</p>
        </div>
        <div className="card">
          <div className="flex items-center gap-2 mb-1">
            <HardDrive size={14} className="text-blue-400" />
            <p className="text-xs text-gray-500">Active Seeds</p>
          </div>
          <p className="font-bold text-blue-300">{activeSeeders}</p>
        </div>
        <div className="card">
          <div className="flex items-center gap-2 mb-1">
            <Users size={14} className="text-purple-400" />
            <p className="text-xs text-gray-500">Connected Peers</p>
          </div>
          <p className="font-bold text-purple-300">
            {torrents.reduce((sum, t) => sum + t.peers, 0)}
          </p>
        </div>
      </div>

      {/* Add torrent form */}
      {showAdd && (
        <div className="card border-vtorrent-700/40">
          <h3 className="font-semibold text-white text-sm mb-3">Add Torrent</h3>
          <div className="space-y-3">
            <div>
              <label className="label">Magnet Link or .torrent URL</label>
              <input
                type="text"
                className="input-field font-mono text-sm"
                placeholder="magnet:?xt=urn:btih:..."
                value={magnetLink}
                onChange={e => setMagnetLink(e.target.value)}
                autoFocus
              />
            </div>
            <div className="flex gap-3">
              <button onClick={() => setShowAdd(false)} className="btn-secondary">Cancel</button>
              <button
                disabled={!magnetLink}
                className="btn-primary"
                onClick={() => {
                  setShowAdd(false)
                  setMagnetLink('')
                }}
              >
                Add & Start
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Incentive explanation */}
      <div className="bg-vtorrent-900/20 border border-vtorrent-800/30 rounded-xl p-4">
        <div className="flex items-start gap-3">
          <Coins size={18} className="text-vtorrent-400 flex-shrink-0 mt-0.5" />
          <div>
            <p className="text-vtorrent-300 font-medium text-sm mb-1">How VTR Incentives Work</p>
            <p className="text-gray-400 text-xs leading-relaxed">
              <strong className="text-gray-300">Seeders earn VTR</strong> — for every GB of data you upload to peers, you receive VTR tokens proportional to your upload ratio.
              <strong className="text-gray-300"> Leechers pay VTR</strong> — to access priority bandwidth from seeders, you can optionally attach a VTR micropayment to your requests.
              All payments are settled via payment channels — no on-chain transaction per transfer.
            </p>
          </div>
        </div>
      </div>

      {/* Torrent list */}
      <div className="card">
        <h2 className="font-semibold text-white text-sm mb-4">Active Torrents</h2>
        <div className="space-y-4">
          {torrents.map(torrent => (
            <div key={torrent.id} className="border border-navy-700/60 rounded-xl p-4">
              {/* Name and status */}
              <div className="flex items-start justify-between gap-3 mb-3">
                <div className="flex-1 min-w-0">
                  <p className="font-medium text-gray-200 text-sm truncate">{torrent.name}</p>
                  <p className="text-xs text-gray-600 font-mono mt-0.5 truncate">{torrent.infoHash}</p>
                </div>
                <div className="flex items-center gap-2 flex-shrink-0">
                  <span className={`text-xs px-2 py-0.5 rounded-full font-medium ${
                    torrent.status === 'seeding'     ? 'bg-emerald-900/40 text-emerald-400' :
                    torrent.status === 'downloading' ? 'bg-blue-900/40 text-blue-400' :
                    torrent.status === 'paused'      ? 'bg-yellow-900/40 text-yellow-400' :
                    'bg-gray-800 text-gray-400'
                  }`}>
                    {torrent.status}
                  </span>
                </div>
              </div>

              {/* Progress bar */}
              <div className="mb-3">
                <div className="flex justify-between text-xs text-gray-500 mb-1">
                  <span>{torrent.progress}% — {formatSize(torrent.size * torrent.progress / 100)} / {formatSize(torrent.size)}</span>
                  <span>{torrent.seeders} seeders · {torrent.peers} peers</span>
                </div>
                <div className="h-1.5 bg-navy-800 rounded-full overflow-hidden">
                  <div
                    className={`h-full rounded-full transition-all ${
                      torrent.status === 'seeding' ? 'bg-emerald-500' : 'bg-vtorrent-500'
                    }`}
                    style={{ width: `${torrent.progress}%` }}
                  />
                </div>
              </div>

              {/* Speed and earnings */}
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-4 text-xs text-gray-500">
                  {torrent.downloadSpeed > 0 && (
                    <span className="flex items-center gap-1">
                      <Download size={11} className="text-blue-400" />
                      {formatSpeed(torrent.downloadSpeed)}
                    </span>
                  )}
                  {torrent.uploadSpeed > 0 && (
                    <span className="flex items-center gap-1">
                      <Upload size={11} className="text-emerald-400" />
                      {formatSpeed(torrent.uploadSpeed)}
                    </span>
                  )}
                </div>
                <div className="flex items-center gap-3">
                  {torrent.earned > 0 && (
                    <span className="flex items-center gap-1 text-xs text-vtorrent-400 font-mono">
                      <Coins size={11} />
                      +{formatVTR(torrent.earned)}
                    </span>
                  )}
                  <div className="flex items-center gap-1">
                    <button className="p-1.5 rounded-lg hover:bg-navy-700 text-gray-500 hover:text-gray-300 transition-colors">
                      {torrent.status === 'paused' ? <Play size={13} /> : <Pause size={13} />}
                    </button>
                    <button className="p-1.5 rounded-lg hover:bg-red-900/20 text-gray-500 hover:text-red-400 transition-colors">
                      <Trash2 size={13} />
                    </button>
                  </div>
                </div>
              </div>
            </div>
          ))}
        </div>
      </div>
    </div>
  )
}
