import { useState } from 'react'
import {
  AlertCircle,
  CheckCircle,
  Copy,
  Cpu,
  RefreshCw,
  RotateCcw,
  Settings,
  ShieldAlert,
} from 'lucide-react'
import { formatInvokeError, isTauri } from '../api'
import {
  getDiagnostics,
  restartApp,
  setBtcPeer,
  useAppInfo,
  useNodeInfo,
} from '../hooks/useNode'

function uptime(seconds: number): string {
  if (seconds < 60) return `${seconds}s`
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`
  return `${Math.floor(seconds / 86400)}d`
}

export default function SettingsPage() {
  const { data: node, error: nodeError } = useNodeInfo(8_000)
  const { data: appInfo, loading: appLoading, error: appError, refresh: refreshAppInfo } = useAppInfo(15_000)
  const [peerDraft, setPeerDraft] = useState<string | null>(null)
  const [savingPeer, setSavingPeer] = useState(false)
  const [peerMessage, setPeerMessage] = useState('')
  const [peerError, setPeerError] = useState('')
  const [diagnostics, setDiagnostics] = useState('')
  const [diagBusy, setDiagBusy] = useState(false)
  const [diagError, setDiagError] = useState('')
  const [copied, setCopied] = useState<string | null>(null)

  const peerValue = peerDraft ?? appInfo?.btcPeer ?? ''

  const copy = async (text: string, label: string) => {
    try {
      await navigator.clipboard.writeText(text)
      setCopied(label)
      window.setTimeout(() => setCopied(null), 1500)
    } catch {
      /* clipboard unavailable */
    }
  }

  const savePeer = async () => {
    setSavingPeer(true)
    setPeerError('')
    setPeerMessage('')
    try {
      await setBtcPeer(peerValue.trim() || null)
      setPeerMessage('BTC peer saved. The next sync cycle uses it.')
      setPeerDraft(null)
      refreshAppInfo()
    } catch (error) {
      setPeerError(formatInvokeError(error, 'Could not save the BTC peer'))
    } finally {
      setSavingPeer(false)
    }
  }

  const previewDiagnostics = async () => {
    setDiagBusy(true)
    setDiagError('')
    setDiagnostics('')
    try {
      setDiagnostics(await getDiagnostics())
    } catch (error) {
      setDiagError(formatInvokeError(error, 'Could not build diagnostics'))
    } finally {
      setDiagBusy(false)
    }
  }

  const handleRestart = async () => {
    if (!window.confirm('Restart the app? The node will stop and reconnect.')) return
    try {
      await restartApp()
    } catch (error) {
      setPeerError(formatInvokeError(error, 'Could not restart the app'))
    }
  }

  return (
    <div className="p-6 max-w-3xl mx-auto space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold text-white flex items-center gap-2">
            <Settings size={20} className="text-vtorrent-400" />
            Settings
          </h1>
          <p className="text-sm text-gray-400 mt-0.5">
            Node information, BTC peer, diagnostics, and restart controls.
          </p>
        </div>
        <button
          onClick={refreshAppInfo}
          disabled={appLoading}
          className="p-2 rounded-lg text-gray-400 hover:text-gray-200 hover:bg-navy-800/60 transition-all"
          title="Refresh settings"
          aria-label="Refresh settings"
        >
          <RefreshCw size={16} className={appLoading ? 'animate-spin' : ''} />
        </button>
      </div>

      {!isTauri() && (
        <div className="card text-sm text-gray-400">
          These settings are available in the desktop app. The browser preview can
          still show node information.
        </div>
      )}

      {appError && isTauri() && (
        <div className="flex items-center gap-2 card text-xs text-red-400">
          <AlertCircle size={14} /> {appError}
        </div>
      )}

      <div className="card space-y-4">
        <h2 className="text-sm font-medium text-gray-300 flex items-center gap-2">
          <Cpu size={15} className="text-vtorrent-400" /> Node
        </h2>
        <div className="grid grid-cols-2 gap-x-8 gap-y-3 text-sm">
          <Row label="Network" value={node?.network ?? appInfo?.network ?? '—'} />
          <Row label="Version" value={node?.version ?? appInfo?.version ?? '—'} />
          <Row label="Block height" value={node ? node.blockHeight.toLocaleString() : '—'} />
          <Row label="Peers" value={node ? String(node.connections) : '—'} />
          <Row
            label="Sync"
            value={node ? (node.syncing ? `${node.syncPercent.toFixed(0)}%` : 'Synced') : '—'}
          />
          <Row label="Mempool" value={node ? String(node.mempoolSize) : '—'} />
          <Row label="Uptime" value={node ? uptime(node.uptimeSecs) : '—'} />
          <Row label="Wallet" value={appInfo?.walletAddress ?? '—'} mono />
        </div>
        <div className="space-y-2 text-xs">
          <PathRow
            label="App data directory"
            value={appInfo?.appDataDir ?? null}
            copied={copied === 'app'}
            onCopy={() => appInfo?.appDataDir && copy(appInfo.appDataDir, 'app')}
          />
          <PathRow
            label="Node data directory"
            value={appInfo?.nodeDataDir ?? null}
            copied={copied === 'node'}
            onCopy={() => appInfo?.nodeDataDir && copy(appInfo.nodeDataDir, 'node')}
          />
        </div>
        {nodeError && <p className="text-xs text-red-400">{nodeError}</p>}
      </div>

      <div className="card space-y-4">
        <h2 className="text-sm font-medium text-gray-300">Bitcoin SPV</h2>
        <div className="grid grid-cols-2 gap-x-8 gap-y-3 text-sm">
          <Row label="Network" value={appInfo?.btcNetwork ?? '—'} />
          <Row label="Peer" value={appInfo?.btcPeer ?? 'DNS seeds'} mono />
        </div>
        <div>
          <label className="label">BTC peer (host:port)</label>
          <div className="flex gap-2">
            <input
              className="input-field font-mono"
              value={peerValue}
              onChange={event => setPeerDraft(event.target.value)}
              placeholder="127.0.0.1:18448"
              disabled={!isTauri() || savingPeer}
            />
            <button
              onClick={savePeer}
              disabled={!isTauri() || savingPeer}
              className="btn-secondary text-xs"
            >
              {savingPeer ? 'Saving…' : 'Save'}
            </button>
          </div>
          <p className="text-xs text-gray-600 mt-1">
            Leave empty to use DNS seeds on mainnet. Testnet and regtest require a peer.
          </p>
          {peerMessage && <p className="text-xs text-emerald-400 mt-1">{peerMessage}</p>}
          {peerError && <p className="text-xs text-red-400 mt-1">{peerError}</p>}
        </div>
      </div>

      <div className="card space-y-4">
        <div className="flex items-center justify-between gap-3">
          <h2 className="text-sm font-medium text-gray-300">Diagnostics</h2>
          <button
            onClick={previewDiagnostics}
            disabled={!isTauri() || diagBusy}
            className="btn-secondary text-xs"
          >
            {diagBusy ? 'Building…' : 'Preview diagnostics'}
          </button>
        </div>
        <p className="text-xs text-gray-500">
          The preview redacts the wallet address and home directory. It never includes
          keys, passphrases, or swap secrets.
        </p>
        {diagError && <p className="text-xs text-red-400">{diagError}</p>}
        {diagnostics && (
          <div className="space-y-2">
            <pre className="text-[11px] text-gray-400 bg-navy-950/60 border border-navy-800 rounded-lg p-3 overflow-x-auto">
              {diagnostics}
            </pre>
            <button
              onClick={() => copy(diagnostics, 'diagnostics')}
              className="btn-secondary text-xs flex items-center gap-1.5"
            >
              {copied === 'diagnostics' ? <CheckCircle size={13} /> : <Copy size={13} />}
              {copied === 'diagnostics' ? 'Copied' : 'Copy diagnostics'}
            </button>
          </div>
        )}
      </div>

      <div className="card space-y-3">
        <h2 className="text-sm font-medium text-gray-300 flex items-center gap-2">
          <ShieldAlert size={15} className="text-amber-400" /> Restart
        </h2>
        <p className="text-xs text-gray-500">
          Restarting stops the node and reloads the app. Use it after changing network
          or BTC peer settings.
        </p>
        <button
          onClick={handleRestart}
          disabled={!isTauri()}
          className="btn-secondary text-xs flex items-center gap-1.5"
        >
          <RotateCcw size={13} /> Restart app
        </button>
      </div>
    </div>
  )
}

function Row({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="flex justify-between gap-4">
      <span className="text-gray-500">{label}</span>
      <span className={`text-gray-300 text-right break-all ${mono ? 'font-mono text-xs' : ''}`}>
        {value}
      </span>
    </div>
  )
}

function PathRow({
  label,
  value,
  copied,
  onCopy,
}: {
  label: string
  value: string | null
  copied: boolean
  onCopy: () => void
}) {
  return (
    <div className="flex items-start justify-between gap-3">
      <span className="text-gray-500">{label}</span>
      <div className="flex items-start gap-2 min-w-0">
        <span className="font-mono text-gray-400 break-all text-right">{value ?? '—'}</span>
        {value && (
          <button
            onClick={onCopy}
            className="p-1 rounded text-gray-600 hover:text-vtorrent-300"
            title={`Copy ${label}`}
          >
            {copied ? <CheckCircle size={12} className="text-emerald-400" /> : <Copy size={12} />}
          </button>
        )}
      </div>
    </div>
  )
}
