import { useState, type ChangeEvent } from 'react'
import { AlertCircle, CheckCircle, FileUp, ShieldCheck } from 'lucide-react'
import { fileToBase64, formatInvokeError } from '../api'
import { useWallet, type BackupPreview } from '../hooks/useWallet'

export interface RestoreResult {
  preview: BackupPreview
  passphrase: string
  otpCode?: string
}

interface RestoreBackupFormProps {
  onRestored: (result: RestoreResult) => Promise<void> | void
}

export default function RestoreBackupForm({ onRestored }: RestoreBackupFormProps) {
  const { verifyBackup, restoreBackup } = useWallet()
  const [fileName, setFileName] = useState('')
  const [backupBase64, setBackupBase64] = useState('')
  const [passphrase, setPassphrase] = useState('')
  const [otpCode, setOtpCode] = useState('')
  const [preview, setPreview] = useState<BackupPreview | null>(null)
  const [confirmOverwrite, setConfirmOverwrite] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [message, setMessage] = useState('')

  const handleFile = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0]
    if (!file) return
    if (file.size > 32 * 1024 * 1024) {
      setError('Backup file is too large')
      return
    }
    setError('')
    setMessage('')
    setPreview(null)
    setFileName(file.name)
    setBackupBase64(await fileToBase64(file))
  }

  const handleVerify = async () => {
    if (!backupBase64) {
      setError('Choose a backup file first')
      return
    }
    setBusy(true)
    setError('')
    setMessage('')
    setPreview(null)
    try {
      const result = await verifyBackup(backupBase64, passphrase, otpCode || undefined)
      setPreview(result)
    } catch (err) {
      setError(formatInvokeError(err, 'Could not verify the backup'))
    } finally {
      setBusy(false)
    }
  }

  const handleRestore = async () => {
    if (!preview || !confirmOverwrite) return
    setBusy(true)
    setError('')
    setMessage('')
    try {
      const restored = await restoreBackup(
        backupBase64,
        passphrase,
        otpCode || undefined,
        true
      )
      setMessage('Backup restored.')
      await onRestored({
        preview: restored,
        passphrase,
        otpCode: otpCode || undefined,
      })
    } catch (err) {
      setError(formatInvokeError(err, 'Could not restore the backup'))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="space-y-4">
      <div>
        <h2 className="font-semibold text-white flex items-center gap-2">
          <ShieldCheck size={16} className="text-vtorrent-400" />
          Restore from backup
        </h2>
        <p className="text-gray-400 text-sm mt-1">
          Restore the encrypted wallet and its recovery state. The backup is verified
          before anything is replaced.
        </p>
      </div>

      <div>
        <label className="label">Backup file</label>
        <label className="flex items-center gap-2 px-3 py-2.5 rounded-lg border border-navy-700 bg-navy-800/60 text-sm text-gray-300 cursor-pointer hover:border-vtorrent-600 transition-colors">
          <FileUp size={15} className="text-vtorrent-400" />
          {fileName || 'Choose a .vtrbackup file'}
          <input
            type="file"
            accept=".vtrbackup,application/json"
            className="hidden"
            onChange={handleFile}
            disabled={busy}
          />
        </label>
      </div>

      <div>
        <label className="label">Wallet passphrase</label>
        <input
          type="password"
          className="input-field"
          value={passphrase}
          onChange={event => setPassphrase(event.target.value)}
          placeholder="Passphrase used when the backup was created"
          disabled={busy}
        />
      </div>

      <div>
        <label className="label">Authenticator code (if the backup wallet has 2FA)</label>
        <input
          type="text"
          inputMode="numeric"
          maxLength={6}
          className="input-field font-mono tracking-widest"
          value={otpCode}
          onChange={event => setOtpCode(event.target.value.replace(/\D/g, ''))}
          placeholder="000000"
          disabled={busy}
        />
      </div>

      {error && (
        <div className="flex items-start gap-2 bg-red-900/20 border border-red-800/30 rounded-lg px-3 py-2.5">
          <AlertCircle size={14} className="text-red-400 flex-shrink-0 mt-0.5" />
          <p className="text-xs text-red-300">{error}</p>
        </div>
      )}

      {message && (
        <div className="flex items-start gap-2 bg-emerald-900/15 border border-emerald-800/30 rounded-lg px-3 py-2.5">
          <CheckCircle size={14} className="text-emerald-400 flex-shrink-0 mt-0.5" />
          <p className="text-xs text-emerald-300">{message}</p>
        </div>
      )}

      {preview && (
        <div className="rounded-lg border border-vtorrent-900/40 bg-navy-900/50 p-3 space-y-2 text-sm">
          <div className="flex justify-between gap-4">
            <span className="text-gray-500">Wallet</span>
            <span className="font-mono text-gray-300 text-right break-all">
              {preview.walletAddress}
            </span>
          </div>
          <div className="flex justify-between gap-4">
            <span className="text-gray-500">Created</span>
            <span className="text-gray-400">
              {new Date(preview.createdAt * 1000).toLocaleString()}
            </span>
          </div>
          <div className="flex justify-between gap-4">
            <span className="text-gray-500">Files</span>
            <span className="text-gray-400">{preview.fileCount}</span>
          </div>
          <div className="flex justify-between gap-4">
            <span className="text-gray-500">2FA</span>
            <span className="text-gray-400">{preview.has2FA ? 'Enabled' : 'Disabled'}</span>
          </div>
          <p className="text-xs text-gray-600 break-all">{preview.files.join(', ')}</p>
        </div>
      )}

      <label className="flex items-start gap-2 text-xs text-gray-400">
        <input
          type="checkbox"
          className="mt-0.5"
          checked={confirmOverwrite}
          onChange={event => setConfirmOverwrite(event.target.checked)}
          disabled={!preview || busy}
        />
        I understand this replaces the current wallet and recovery state. A rollback copy
        of the current wallet is kept in the backups folder.
      </label>

      <p className="text-xs text-gray-600">
        Restore requires the node to be stopped. If the node is running, lock the wallet
        and restart the app, then restore from the welcome screen.
      </p>

      <div className="flex gap-3">
        <button
          type="button"
          onClick={handleVerify}
          disabled={!backupBase64 || !passphrase || busy}
          className="flex-1 btn-secondary"
        >
          {busy && !preview ? 'Verifying…' : 'Verify backup'}
        </button>
        <button
          type="button"
          onClick={handleRestore}
          disabled={!preview || !confirmOverwrite || busy}
          className="flex-1 btn-primary"
        >
          {busy && preview ? 'Restoring…' : 'Restore backup'}
        </button>
      </div>
    </div>
  )
}
