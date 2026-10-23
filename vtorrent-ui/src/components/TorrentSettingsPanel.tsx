import { useState } from 'react'
import { AlertCircle, CheckCircle, Gauge, RefreshCw } from 'lucide-react'
import { formatInvokeError } from '../api'
import { setTorrentSettings, useTorrentSettings } from '../hooks/useNode'

export default function TorrentSettingsPanel() {
  const { data: settings, loading, error, refresh } = useTorrentSettings(30_000)
  const [dirDraft, setDirDraft] = useState<string | null>(null)
  const [downloadDraft, setDownloadDraft] = useState<string | null>(null)
  const [uploadDraft, setUploadDraft] = useState<string | null>(null)
  const [ratioDraft, setRatioDraft] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [message, setMessage] = useState('')
  const [saveError, setSaveError] = useState('')

  const dirValue = dirDraft ?? settings?.downloadDir ?? ''
  const downloadValue = downloadDraft
    ?? String(Math.round((settings?.downloadLimitBytesPerSec ?? 0) / 1024))
  const uploadValue = uploadDraft
    ?? String(Math.round((settings?.uploadLimitBytesPerSec ?? 0) / 1024))
  const ratioValue = ratioDraft ?? String(settings?.seedRatioTarget ?? 0)

  const save = async () => {
    setSaving(true)
    setMessage('')
    setSaveError('')
    try {
      const updated = await setTorrentSettings({
        downloadDir: dirValue.trim(),
        downloadLimitBytesPerSec: Math.max(0, Math.round(Number(downloadValue) * 1024)),
        uploadLimitBytesPerSec: Math.max(0, Math.round(Number(uploadValue) * 1024)),
        seedRatioTarget: Math.max(0, Number(ratioValue) || 0),
      })
      setMessage('Torrent settings saved.')
      setDirDraft(updated.downloadDir)
      setDownloadDraft(String(Math.round(updated.downloadLimitBytesPerSec / 1024)))
      setUploadDraft(String(Math.round(updated.uploadLimitBytesPerSec / 1024)))
      setRatioDraft(String(updated.seedRatioTarget))
      refresh()
    } catch (err) {
      setSaveError(formatInvokeError(err, 'Could not save torrent settings'))
    } finally {
      setSaving(false)
    }
  }

  return (
    <div className="card space-y-3">
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-sm font-medium text-gray-300 flex items-center gap-2">
          <Gauge size={15} className="text-vtorrent-400" /> Torrent settings
        </h2>
        <button
          onClick={refresh}
          disabled={loading}
          className="p-1.5 rounded-lg text-gray-500 hover:text-gray-300 hover:bg-navy-800"
          title="Refresh torrent settings"
        >
          <RefreshCw size={13} className={loading ? 'animate-spin' : ''} />
        </button>
      </div>

      <div className="grid gap-3 sm:grid-cols-2">
        <div className="sm:col-span-2">
          <label className="label">Download directory</label>
          <input
            data-testid="torrent-download-dir"
            className="input-field font-mono text-xs"
            value={dirValue}
            onChange={event => setDirDraft(event.target.value)}
            placeholder="/path/to/downloads"
            disabled={saving}
          />
        </div>
        <div>
          <label className="label">Download limit (KB/s)</label>
          <input
            data-testid="torrent-download-limit"
            className="input-field font-mono text-xs"
            inputMode="numeric"
            value={downloadValue}
            onChange={event => setDownloadDraft(event.target.value.replace(/[^\d]/g, ''))}
            disabled={saving}
          />
        </div>
        <div>
          <label className="label">Upload limit (KB/s)</label>
          <input
            data-testid="torrent-upload-limit"
            className="input-field font-mono text-xs"
            inputMode="numeric"
            value={uploadValue}
            onChange={event => setUploadDraft(event.target.value.replace(/[^\d]/g, ''))}
            disabled={saving}
          />
        </div>
        <div>
          <label className="label">Seed ratio target</label>
          <input
            data-testid="torrent-seed-ratio"
            className="input-field font-mono text-xs"
            inputMode="decimal"
            value={ratioValue}
            onChange={event => setRatioDraft(event.target.value)}
            placeholder="0 = disabled"
            disabled={saving}
          />
        </div>
        <div className="flex items-end">
          <button
            data-testid="torrent-settings-save"
            onClick={save}
            disabled={saving || !dirValue.trim()}
            className="btn-secondary text-xs"
          >
            {saving ? 'Saving…' : 'Save torrent settings'}
          </button>
        </div>
      </div>

      <p className="text-xs text-gray-600">
        0 means unlimited. The seed ratio target stops uploads once the session
        reaches that upload/download ratio.
      </p>

      {error && <p className="text-xs text-red-400">{error}</p>}
      {saveError && (
        <p className="text-xs text-red-400 flex items-center gap-1.5">
          <AlertCircle size={12} /> {saveError}
        </p>
      )}
      {message && (
        <p className="text-xs text-emerald-400 flex items-center gap-1.5">
          <CheckCircle size={12} /> {message}
        </p>
      )}
    </div>
  )
}
