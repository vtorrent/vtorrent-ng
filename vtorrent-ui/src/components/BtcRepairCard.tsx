import { useState } from 'react'
import { AlertCircle, CheckCircle, RotateCcw, ShieldAlert } from 'lucide-react'
import { formatInvokeError } from '../api'
import { useWallet } from '../hooks/useWallet'

interface BtcRepairCardProps {
  title?: string
  description?: string
  onRepaired?: (message: string) => void
}

const DEFAULT_DESCRIPTION =
  'Only for a wallet imported before the BTC-state fix that now reports a missing BTC state. This attests that the wallet has no prior BTC history and creates fresh mainnet and regtest state. It refuses to overwrite an existing state.'

export default function BtcRepairCard({
  title = 'Legacy BTC state repair',
  description = DEFAULT_DESCRIPTION,
  onRepaired,
}: BtcRepairCardProps) {
  const { repairBtcState } = useWallet()
  const [passphrase, setPassphrase] = useState('')
  const [otp, setOtp] = useState('')
  const [attest, setAttest] = useState(false)
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState('')
  const [error, setError] = useState('')

  const handleRepair = async () => {
    if (!attest) {
      setError('Confirm that this wallet has no prior BTC history first.')
      return
    }
    setBusy(true)
    setMessage('')
    setError('')
    try {
      const result = await repairBtcState(passphrase, otp || undefined, true)
      setMessage(result)
      setPassphrase('')
      setOtp('')
      setAttest(false)
      onRepaired?.(result)
    } catch (cause) {
      setError(formatInvokeError(cause, 'BTC state repair failed'))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="card space-y-3">
      <h2 className="text-sm font-medium text-gray-300 flex items-center gap-2">
        <ShieldAlert size={15} className="text-amber-400" /> {title}
      </h2>
      <p className="text-xs text-gray-500">{description}</p>
      <input
        type="password"
        value={passphrase}
        onChange={e => setPassphrase(e.target.value)}
        placeholder="Wallet passphrase"
        className="input text-xs w-full"
        autoComplete="current-password"
      />
      <input
        type="text"
        value={otp}
        onChange={e => setOtp(e.target.value)}
        placeholder="2FA code (if enabled)"
        className="input text-xs w-full"
        autoComplete="one-time-code"
      />
      <label className="flex items-start gap-2 text-xs text-gray-400">
        <input
          type="checkbox"
          checked={attest}
          onChange={e => setAttest(e.target.checked)}
          className="mt-0.5"
        />
        I attest this wallet has no prior BTC history or outstanding BTC sends.
      </label>
      <button
        onClick={handleRepair}
        disabled={busy || !passphrase || !attest}
        className="btn-secondary text-xs flex items-center gap-1.5"
      >
        <RotateCcw size={13} /> {busy ? 'Repairing…' : 'Repair BTC state'}
      </button>
      {message && (
        <p className="text-xs text-emerald-400 flex items-center gap-1.5">
          <CheckCircle size={13} /> {message}
        </p>
      )}
      {error && (
        <p className="text-xs text-red-400 flex items-center gap-1.5">
          <AlertCircle size={13} /> {error}
        </p>
      )}
    </div>
  )
}
