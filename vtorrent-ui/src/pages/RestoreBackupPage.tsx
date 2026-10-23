import { useNavigate } from 'react-router-dom'
import { ArrowLeft, ShieldCheck } from 'lucide-react'
import { useWallet } from '../hooks/useWallet'
import { useNetwork, parseSeeds } from '../hooks/useNetwork'
import NetworkPicker from '../components/NetworkPicker'
import RestoreBackupForm, { type RestoreResult } from '../components/RestoreBackupForm'

export default function RestoreBackupPage() {
  const navigate = useNavigate()
  const { unlock } = useWallet()
  const { network, setNetwork, seeds, setSeeds } = useNetwork()

  const handleRestored = async ({ passphrase, otpCode }: RestoreResult) => {
    try {
      await unlock(passphrase, otpCode, { network, seeds: parseSeeds(seeds) })
      navigate('/dashboard')
    } catch {
      navigate('/')
    }
  }

  return (
    <div className="min-h-screen gradient-bg flex flex-col items-center justify-center p-6">
      <div className="w-full max-w-md mb-6">
        <button
          onClick={() => navigate('/')}
          className="flex items-center gap-2 text-gray-500 hover:text-gray-300 text-sm transition-colors"
        >
          <ArrowLeft size={14} /> Back
        </button>
      </div>

      <div className="w-full max-w-md card">
        <h2 className="text-lg font-semibold text-white mb-1 flex items-center gap-2">
          <ShieldCheck size={18} className="text-emerald-400" />
          Restore Wallet
        </h2>
        <p className="text-gray-400 text-sm mb-6">
          Restore an encrypted vTorrent backup on this device.
        </p>

        <div className="mb-5">
          <NetworkPicker
            network={network}
            onChange={setNetwork}
            seeds={seeds}
            onSeedsChange={setSeeds}
          />
        </div>

        <RestoreBackupForm onRestored={handleRestored} />
      </div>
    </div>
  )
}
