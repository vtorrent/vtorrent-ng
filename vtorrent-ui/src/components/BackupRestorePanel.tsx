import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { AlertCircle, CheckCircle, Download, Lock } from 'lucide-react'
import { formatInvokeError } from '../api'
import { useWallet, type BackupInfo } from '../hooks/useWallet'
import RestoreBackupForm from './RestoreBackupForm'

export default function BackupRestorePanel() {
  const navigate = useNavigate()
  const { createBackup, has2FA, lock } = useWallet()
  const [passphrase, setPassphrase] = useState('')
  const [otpCode, setOtpCode] = useState('')
  const [info, setInfo] = useState<BackupInfo | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [message, setMessage] = useState('')

  const handleCreate = async () => {
    setBusy(true)
    setError('')
    setMessage('')
    setInfo(null)
    try {
      const result = await createBackup(passphrase, otpCode || undefined)
      setInfo(result)
      setMessage('Backup created. Copy the file off this device.')
      setPassphrase('')
      setOtpCode('')
    } catch (err) {
      setError(formatInvokeError(err, 'Could not create the backup'))
    } finally {
      setBusy(false)
    }
  }

  const handleRestored = async () => {
    await lock()
    navigate('/')
  }

  return (
    <div className="card space-y-6">
      <div className="space-y-4">
        <div>
          <h2 className="font-semibold text-white flex items-center gap-2">
            <Download size={16} className="text-vtorrent-400" />
            Create encrypted backup
          </h2>
          <p className="text-gray-400 text-sm mt-1">
            Includes the encrypted wallet, cold-stake registry, swap recovery records,
            and wallet-scoped BTC state. Keep the backup off this device.
          </p>
          <p className="text-xs text-amber-300/90 mt-2">
            2FA protects app unlock, not the backup. The backup is encrypted with the
            wallet passphrase; use a strong passphrase and keep the backup offline.
          </p>
        </div>

        <div>
          <label className="label">Wallet passphrase</label>
          <input
            type="password"
            className="input-field"
            value={passphrase}
            onChange={event => setPassphrase(event.target.value)}
            placeholder="Encrypts the backup"
            disabled={busy}
          />
        </div>

        {has2FA && (
          <div>
            <label className="label">Authenticator code</label>
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
        )}

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

        {info && (
          <div className="rounded-lg border border-vtorrent-900/40 bg-navy-900/50 p-3 space-y-1.5 text-sm">
            <p className="text-gray-500 text-xs">Backup path</p>
            <p className="font-mono text-xs text-vtorrent-300 break-all">{info.path}</p>
            <p className="text-xs text-gray-500">
              {info.fileCount} files · {info.size.toLocaleString()} bytes
            </p>
          </div>
        )}

        <button
          type="button"
          onClick={handleCreate}
          disabled={!passphrase || busy}
          className="btn-primary"
        >
          {busy ? 'Creating backup…' : 'Create backup'}
        </button>
      </div>

      <div className="border-t border-navy-800 pt-6">
        <RestoreBackupForm onRestored={handleRestored} />
      </div>

      <div className="flex items-start gap-2 text-xs text-gray-600">
        <Lock size={13} className="mt-0.5 flex-shrink-0" />
        <p>
          Restoring replaces the current wallet. The app returns to the welcome screen
          so you can open the restored wallet.
        </p>
      </div>
    </div>
  )
}
