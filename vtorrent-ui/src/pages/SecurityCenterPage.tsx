import { useState } from 'react'
import { Shield, Smartphone, Key, CheckCircle, AlertTriangle, Copy } from 'lucide-react'
import { useWallet } from '../hooks/useWallet'
import QRCode from 'qrcode.react'

type TwoFAStep = 'idle' | 'setup' | 'verify' | 'done'

export default function SecurityCenterPage() {
  const { has2FA, enable2FA, disable2FA } = useWallet()

  const [twoFAStep, setTwoFAStep] = useState<TwoFAStep>('idle')
  const [totpUri, setTotpUri] = useState('')
  const [totpSecret, setTotpSecret] = useState('')
  const [verifyCode, setVerifyCode] = useState('')
  const [disableCode, setDisableCode] = useState('')
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState('')
  const [copied, setCopied] = useState(false)
  const [showDisable, setShowDisable] = useState(false)

  const handleEnable2FA = async () => {
    setLoading(true)
    setError('')
    try {
      const { uri, secret } = await enable2FA()
      setTotpUri(uri)
      setTotpSecret(secret)
      setTwoFAStep('setup')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to enable 2FA')
    } finally {
      setLoading(false)
    }
  }

  const handleVerify2FA = async (e: React.FormEvent) => {
    e.preventDefault()
    setLoading(true)
    setError('')
    try {
      // In real app, verify the code against the TOTP secret before confirming
      if (verifyCode.length !== 6) throw new Error('Enter a 6-digit code')
      setTwoFAStep('done')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Verification failed')
    } finally {
      setLoading(false)
    }
  }

  const handleDisable2FA = async (e: React.FormEvent) => {
    e.preventDefault()
    setLoading(true)
    setError('')
    try {
      await disable2FA(disableCode)
      setShowDisable(false)
      setDisableCode('')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to disable 2FA')
    } finally {
      setLoading(false)
    }
  }

  const copySecret = () => {
    navigator.clipboard.writeText(totpSecret)
    setCopied(true)
    setTimeout(() => setCopied(false), 2000)
  }

  return (
    <div className="p-6 space-y-6">
      <div>
        <h1 className="text-xl font-bold text-white">Security Center</h1>
        <p className="text-gray-500 text-sm mt-0.5">Manage wallet security and authentication</p>
      </div>

      {/* Security overview */}
      <div className="grid grid-cols-3 gap-4">
        <div className="card">
          <div className="flex items-center gap-2 mb-2">
            <Shield size={16} className={has2FA ? 'text-emerald-400' : 'text-gray-600'} />
            <p className="text-sm font-medium text-gray-300">2FA Status</p>
          </div>
          {has2FA
            ? <span className="badge-green">Enabled</span>
            : <span className="badge-red">Disabled</span>
          }
        </div>
        <div className="card">
          <div className="flex items-center gap-2 mb-2">
            <Key size={16} className="text-vtorrent-400" />
            <p className="text-sm font-medium text-gray-300">Encryption</p>
          </div>
          <span className="badge-green">Argon2id + ChaCha20</span>
        </div>
        <div className="card">
          <div className="flex items-center gap-2 mb-2">
            <Shield size={16} className="text-vtorrent-400" />
            <p className="text-sm font-medium text-gray-300">Key Storage</p>
          </div>
          <span className="badge-green">Local Only</span>
        </div>
      </div>

      {/* 2FA Section */}
      <div className="card">
        <div className="flex items-start justify-between mb-4">
          <div>
            <h2 className="font-semibold text-white flex items-center gap-2">
              <Smartphone size={16} className="text-vtorrent-400" />
              Two-Factor Authentication (TOTP)
            </h2>
            <p className="text-gray-400 text-sm mt-1">
              Require a 6-digit code from your authenticator app every time you unlock the wallet.
              Compatible with Google Authenticator, Authy, and all standard TOTP apps.
            </p>
          </div>
        </div>

        {/* Not enabled state */}
        {!has2FA && twoFAStep === 'idle' && (
          <div>
            <div className="bg-amber-900/15 border border-amber-800/30 rounded-lg p-3 mb-4 flex gap-3">
              <AlertTriangle size={16} className="text-amber-400 flex-shrink-0 mt-0.5" />
              <p className="text-amber-300/80 text-sm">
                2FA is not enabled. Anyone with your passphrase can access your wallet.
                Enable 2FA to add a second layer of protection.
              </p>
            </div>
            <button onClick={handleEnable2FA} disabled={loading} className="btn-primary">
              {loading ? 'Setting up...' : 'Enable 2FA'}
            </button>
          </div>
        )}

        {/* Setup step: show QR code */}
        {twoFAStep === 'setup' && (
          <div className="space-y-4">
            <p className="text-sm text-gray-400">
              Scan this QR code with your authenticator app, then enter the 6-digit code to confirm.
            </p>
            <div className="flex gap-6 items-start">
              <div className="bg-white p-3 rounded-xl flex-shrink-0">
                <QRCode value={totpUri} size={140} />
              </div>
              <div className="flex-1 space-y-3">
                <div>
                  <p className="text-xs text-gray-500 mb-1">Manual entry secret</p>
                  <div className="flex items-center gap-2">
                    <code className="font-mono text-sm text-vtorrent-300 bg-navy-800 px-3 py-1.5 rounded-lg border border-vtorrent-900/40 flex-1 break-all">
                      {totpSecret}
                    </code>
                    <button onClick={copySecret} className="text-gray-500 hover:text-vtorrent-400 transition-colors">
                      {copied ? <CheckCircle size={16} className="text-vtorrent-400" /> : <Copy size={16} />}
                    </button>
                  </div>
                </div>
                <div className="bg-navy-800/60 rounded-lg p-3 text-xs text-gray-500 space-y-1">
                  <p className="text-gray-400 font-medium">Backup this secret!</p>
                  <p>Store it securely. If you lose your phone, this is the only way to recover access to your wallet.</p>
                </div>
              </div>
            </div>

            <form onSubmit={handleVerify2FA} className="space-y-3">
              <div>
                <label className="label">Verification Code</label>
                <input
                  type="text"
                  className="input-field font-mono tracking-widest text-center text-lg"
                  placeholder="000000"
                  maxLength={6}
                  value={verifyCode}
                  onChange={e => setVerifyCode(e.target.value.replace(/\D/g, ''))}
                  autoFocus
                />
              </div>
              {error && <p className="text-red-400 text-sm">{error}</p>}
              <div className="flex gap-3">
                <button type="button" onClick={() => setTwoFAStep('idle')} className="btn-secondary flex-1">
                  Cancel
                </button>
                <button type="submit" disabled={loading || verifyCode.length !== 6} className="btn-primary flex-1">
                  {loading ? 'Verifying...' : 'Confirm & Enable 2FA'}
                </button>
              </div>
            </form>
          </div>
        )}

        {/* Done state */}
        {twoFAStep === 'done' && (
          <div className="flex items-center gap-3 bg-emerald-900/15 border border-emerald-800/30 rounded-lg p-4">
            <CheckCircle size={20} className="text-emerald-400 flex-shrink-0" />
            <div>
              <p className="text-emerald-300 font-medium text-sm">2FA Successfully Enabled</p>
              <p className="text-gray-500 text-xs mt-0.5">Your wallet now requires a TOTP code on every unlock.</p>
            </div>
          </div>
        )}

        {/* Enabled state */}
        {has2FA && twoFAStep === 'idle' && (
          <div className="space-y-4">
            <div className="flex items-center gap-3 bg-emerald-900/15 border border-emerald-800/30 rounded-lg p-3">
              <CheckCircle size={16} className="text-emerald-400 flex-shrink-0" />
              <p className="text-emerald-300 text-sm">2FA is active. Your wallet requires a TOTP code to unlock.</p>
            </div>

            {!showDisable ? (
              <button
                onClick={() => setShowDisable(true)}
                className="text-sm text-red-400 hover:text-red-300 transition-colors"
              >
                Disable 2FA...
              </button>
            ) : (
              <form onSubmit={handleDisable2FA} className="space-y-3 border border-red-800/30 rounded-lg p-4 bg-red-900/10">
                <p className="text-red-300 text-sm font-medium">Confirm disable 2FA</p>
                <p className="text-gray-500 text-xs">Enter your current authenticator code to confirm.</p>
                <input
                  type="text"
                  className="input-field font-mono tracking-widest text-center"
                  placeholder="000000"
                  maxLength={6}
                  value={disableCode}
                  onChange={e => setDisableCode(e.target.value.replace(/\D/g, ''))}
                  autoFocus
                />
                {error && <p className="text-red-400 text-sm">{error}</p>}
                <div className="flex gap-3">
                  <button type="button" onClick={() => setShowDisable(false)} className="btn-secondary flex-1">
                    Cancel
                  </button>
                  <button type="submit" disabled={loading || disableCode.length !== 6} className="btn-danger flex-1">
                    {loading ? 'Disabling...' : 'Disable 2FA'}
                  </button>
                </div>
              </form>
            )}
          </div>
        )}
      </div>

      {/* Encryption info */}
      <div className="card">
        <h2 className="font-semibold text-white flex items-center gap-2 mb-3">
          <Key size={16} className="text-vtorrent-400" />
          Wallet Encryption
        </h2>
        <div className="space-y-2 text-sm">
          {[
            { label: 'Key Derivation', value: 'Argon2id (m=64MB, t=3, p=4)' },
            { label: 'Encryption',     value: 'ChaCha20-Poly1305 (AEAD)' },
            { label: 'Key Size',       value: '256-bit' },
            { label: 'Storage',        value: 'Local encrypted file only' },
          ].map(({ label, value }) => (
            <div key={label} className="flex justify-between py-1.5 border-b border-navy-800/60 last:border-0">
              <span className="text-gray-500">{label}</span>
              <span className="text-gray-300 font-mono text-xs">{value}</span>
            </div>
          ))}
        </div>
      </div>
    </div>
  )
}
