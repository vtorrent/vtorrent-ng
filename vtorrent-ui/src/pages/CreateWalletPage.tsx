import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { ArrowLeft, ArrowRight, PlusCircle, Shield, Eye, EyeOff } from 'lucide-react'
import { useWallet } from '../hooks/useWallet'

export default function CreateWalletPage() {
  const navigate = useNavigate()
  const { createWallet } = useWallet()
  const [passphrase, setPassphrase] = useState('')
  const [confirm, setConfirm] = useState('')
  const [showPass, setShowPass] = useState(false)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState('')

  const strength = passphrase.length === 0 ? 0
    : passphrase.length < 8 ? 1
    : passphrase.length < 14 ? 2
    : passphrase.length < 20 ? 3
    : 4

  const strengthLabel = ['', 'Weak', 'Fair', 'Good', 'Strong'][strength]
  const strengthColor = ['', 'bg-red-500', 'bg-yellow-500', 'bg-blue-500', 'bg-emerald-500'][strength]

  const handleCreate = async (e: React.FormEvent) => {
    e.preventDefault()
    if (passphrase !== confirm) {
      setError('Passphrases do not match')
      return
    }
    if (passphrase.length < 8) {
      setError('Passphrase must be at least 8 characters')
      return
    }
    setLoading(true)
    setError('')
    try {
      await createWallet(passphrase)
      navigate('/dashboard')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to create wallet')
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className="min-h-screen gradient-bg flex flex-col items-center justify-center p-6">
      <div className="w-full max-w-md mb-6">
        <button onClick={() => navigate('/')} className="flex items-center gap-2 text-gray-500 hover:text-gray-300 text-sm transition-colors">
          <ArrowLeft size={14} /> Back
        </button>
      </div>

      <div className="w-full max-w-md card">
        <h2 className="text-lg font-semibold text-white mb-1 flex items-center gap-2">
          <PlusCircle size={18} className="text-emerald-400" />
          Create New Wallet
        </h2>
        <p className="text-gray-400 text-sm mb-6">
          Choose a strong passphrase. This encrypts your wallet file using Argon2id + ChaCha20-Poly1305.
        </p>

        <form onSubmit={handleCreate} className="space-y-4">
          <div>
            <label className="label">Passphrase</label>
            <div className="relative">
              <input
                type={showPass ? 'text' : 'password'}
                className="input-field pr-10"
                placeholder="Choose a strong passphrase"
                value={passphrase}
                onChange={e => setPassphrase(e.target.value)}
                autoFocus
                required
              />
              <button
                type="button"
                onClick={() => setShowPass(!showPass)}
                className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-500 hover:text-gray-300"
              >
                {showPass ? <EyeOff size={16} /> : <Eye size={16} />}
              </button>
            </div>
            {passphrase && (
              <div className="mt-2 flex items-center gap-2">
                <div className="flex-1 h-1 bg-navy-700 rounded-full overflow-hidden">
                  <div
                    className={`h-full rounded-full transition-all duration-300 ${strengthColor}`}
                    style={{ width: `${strength * 25}%` }}
                  />
                </div>
                <span className={`text-xs font-medium ${
                  strength <= 1 ? 'text-red-400' : strength === 2 ? 'text-yellow-400' :
                  strength === 3 ? 'text-blue-400' : 'text-emerald-400'
                }`}>{strengthLabel}</span>
              </div>
            )}
          </div>

          <div>
            <label className="label">Confirm Passphrase</label>
            <input
              type="password"
              className="input-field"
              placeholder="Repeat your passphrase"
              value={confirm}
              onChange={e => setConfirm(e.target.value)}
              required
            />
          </div>

          {error && (
            <p className="text-red-400 text-sm bg-red-900/20 border border-red-800/40 rounded-lg px-3 py-2">
              {error}
            </p>
          )}

          <div className="flex gap-2 bg-vtorrent-900/20 border border-vtorrent-800/30 rounded-lg p-3">
            <Shield size={14} className="text-vtorrent-400 flex-shrink-0 mt-0.5" />
            <p className="text-vtorrent-300/80 text-xs leading-relaxed">
              You can enable TOTP 2FA after creating your wallet from the Security Center.
              This adds a second layer of protection requiring your phone to unlock the wallet.
            </p>
          </div>

          <button
            type="submit"
            disabled={loading || !passphrase || !confirm}
            className="btn-primary w-full"
          >
            {loading ? 'Creating...' : 'Create Wallet'}
            {!loading && <ArrowRight size={16} className="inline ml-2" />}
          </button>
        </form>
      </div>
    </div>
  )
}
