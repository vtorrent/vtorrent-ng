import { useNavigate } from 'react-router-dom'
import { useState } from 'react'
import { Download, PlusCircle, Shield, ArrowRight, Lock } from 'lucide-react'
import { useWallet } from '../hooks/useWallet'

export default function WelcomePage() {
  const navigate = useNavigate()
  const { unlock } = useWallet()
  const [passphrase, setPassphrase] = useState('')
  const [otpCode, setOtpCode] = useState('')
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState('')
  const [showUnlock, setShowUnlock] = useState(false)

  const handleUnlock = async (e: React.FormEvent) => {
    e.preventDefault()
    setLoading(true)
    setError('')
    try {
      await unlock(passphrase, otpCode || undefined)
      navigate('/dashboard')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Incorrect passphrase or OTP code')
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className="min-h-screen gradient-bg flex flex-col items-center justify-center p-6">
      {/* Header */}
      <div className="text-center mb-10">
        <div className="inline-flex items-center justify-center w-16 h-16 rounded-2xl bg-vtorrent-500/15 border border-vtorrent-500/30 mb-5">
          <span className="text-vtorrent-400 font-bold text-2xl">VT</span>
        </div>
        <h1 className="text-3xl font-bold text-white mb-2">vTorrent 2.0</h1>
        <p className="text-gray-400 text-sm max-w-xs mx-auto">
          The decentralized torrent economy. Earn VTR for seeding. Trade peer-to-peer. No exchanges needed.
        </p>
      </div>

      {!showUnlock ? (
        /* Action cards */
        <div className="w-full max-w-sm space-y-3">
          {/* Open existing wallet */}
          <button
            onClick={() => setShowUnlock(true)}
            className="w-full card hover:border-vtorrent-700/60 transition-all duration-200 text-left group"
          >
            <div className="flex items-center gap-4">
              <div className="w-10 h-10 rounded-lg bg-vtorrent-500/15 border border-vtorrent-500/30 flex items-center justify-center flex-shrink-0">
                <Lock size={18} className="text-vtorrent-400" />
              </div>
              <div className="flex-1">
                <p className="font-semibold text-white text-sm">Open Wallet</p>
                <p className="text-gray-500 text-xs mt-0.5">Unlock your existing vTorrent 2.0 wallet</p>
              </div>
              <ArrowRight size={16} className="text-gray-600 group-hover:text-vtorrent-400 transition-colors" />
            </div>
          </button>

          {/* Import legacy wallet */}
          <button
            onClick={() => navigate('/import')}
            className="w-full card hover:border-vtorrent-700/60 transition-all duration-200 text-left group"
          >
            <div className="flex items-center gap-4">
              <div className="w-10 h-10 rounded-lg bg-amber-500/10 border border-amber-500/20 flex items-center justify-center flex-shrink-0">
                <Download size={18} className="text-amber-400" />
              </div>
              <div className="flex-1">
                <p className="font-semibold text-white text-sm">Import Legacy Wallet</p>
                <p className="text-gray-500 text-xs mt-0.5">Claim your old VTR from a wallet.dat file</p>
              </div>
              <ArrowRight size={16} className="text-gray-600 group-hover:text-amber-400 transition-colors" />
            </div>
          </button>

          {/* Create new wallet */}
          <button
            onClick={() => navigate('/create')}
            className="w-full card hover:border-vtorrent-700/60 transition-all duration-200 text-left group"
          >
            <div className="flex items-center gap-4">
              <div className="w-10 h-10 rounded-lg bg-emerald-500/10 border border-emerald-500/20 flex items-center justify-center flex-shrink-0">
                <PlusCircle size={18} className="text-emerald-400" />
              </div>
              <div className="flex-1">
                <p className="font-semibold text-white text-sm">Create New Wallet</p>
                <p className="text-gray-500 text-xs mt-0.5">Start fresh with a new VTR wallet</p>
              </div>
              <ArrowRight size={16} className="text-gray-600 group-hover:text-emerald-400 transition-colors" />
            </div>
          </button>

          {/* Feature highlights */}
          <div className="pt-4 grid grid-cols-3 gap-2 text-center">
            {[
              { icon: Shield, label: 'Built-in 2FA' },
              { icon: Download, label: 'Earn by Seeding' },
              { icon: ArrowRight, label: 'P2P Trading' },
            ].map(({ icon: Icon, label }) => (
              <div key={label} className="flex flex-col items-center gap-1.5 p-2">
                <Icon size={14} className="text-vtorrent-500" />
                <span className="text-xs text-gray-500">{label}</span>
              </div>
            ))}
          </div>
        </div>
      ) : (
        /* Unlock form */
        <div className="w-full max-w-sm">
          <div className="card">
            <h2 className="text-lg font-semibold text-white mb-5 flex items-center gap-2">
              <Lock size={18} className="text-vtorrent-400" />
              Unlock Wallet
            </h2>

            <form onSubmit={handleUnlock} className="space-y-4">
              <div>
                <label className="label">Passphrase</label>
                <input
                  type="password"
                  className="input-field"
                  placeholder="Enter your wallet passphrase"
                  value={passphrase}
                  onChange={e => setPassphrase(e.target.value)}
                  autoFocus
                  required
                />
              </div>

              <div>
                <label className="label">
                  2FA Code
                  <span className="text-gray-600 font-normal ml-1">(if enabled)</span>
                </label>
                <input
                  type="text"
                  className="input-field font-mono tracking-widest"
                  placeholder="000000"
                  maxLength={6}
                  value={otpCode}
                  onChange={e => setOtpCode(e.target.value.replace(/\D/g, ''))}
                />
              </div>

              {error && (
                <p className="text-red-400 text-sm bg-red-900/20 border border-red-800/40 rounded-lg px-3 py-2">
                  {error}
                </p>
              )}

              <div className="flex gap-3 pt-1">
                <button
                  type="button"
                  onClick={() => setShowUnlock(false)}
                  className="btn-secondary flex-1"
                >
                  Back
                </button>
                <button
                  type="submit"
                  disabled={loading || !passphrase}
                  className="btn-primary flex-1"
                >
                  {loading ? 'Unlocking...' : 'Unlock'}
                </button>
              </div>
            </form>
          </div>
        </div>
      )}
    </div>
  )
}
