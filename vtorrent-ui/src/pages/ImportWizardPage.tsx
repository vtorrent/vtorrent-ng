import { useState, useCallback } from 'react'
import { useNavigate } from 'react-router-dom'
import {
  Upload, CheckCircle, AlertCircle, ArrowRight, ArrowLeft,
  Key, Shield, Coins, FileText
} from 'lucide-react'
import { useWallet, formatVTR, type ImportResult } from '../hooks/useWallet'

type Step = 'upload' | 'passphrase' | 'importing' | 'result'

export default function ImportWizardPage() {
  const navigate = useNavigate()
  const { importLegacyWallet } = useWallet()

  const [step, setStep] = useState<Step>('upload')
  const [walletFile, setWalletFile] = useState<File | null>(null)
  const [walletBase64, setWalletBase64] = useState('')
  const [passphrase, setPassphrase] = useState('')
  const [error, setError] = useState('')
  const [result, setResult] = useState<ImportResult | null>(null)

  const handleFileSelect = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0]
    if (!file) return
    setWalletFile(file)
    setError('')

    const reader = new FileReader()
    reader.onload = (ev) => {
      const base64 = btoa(
        new Uint8Array(ev.target?.result as ArrayBuffer)
          .reduce((data, byte) => data + String.fromCharCode(byte), '')
      )
      setWalletBase64(base64)
    }
    reader.readAsArrayBuffer(file)
  }, [])

  const handleFileDrop = useCallback((e: React.DragEvent) => {
    e.preventDefault()
    const file = e.dataTransfer.files[0]
    if (file) {
      const syntheticEvent = { target: { files: [file] } } as any
      handleFileSelect(syntheticEvent)
    }
  }, [handleFileSelect])

  const handleImport = async () => {
    setStep('importing')
    setError('')
    try {
      const importResult = await importLegacyWallet(walletBase64, passphrase || undefined)
      setResult(importResult)
      setStep('result')
    } catch (err: any) {
      setError(err.message || 'Import failed')
      setStep('passphrase')
    }
  }

  return (
    <div className="min-h-screen gradient-bg flex flex-col items-center justify-center p-6">
      {/* Header */}
      <div className="w-full max-w-lg mb-6">
        <button
          onClick={() => navigate('/')}
          className="flex items-center gap-2 text-gray-500 hover:text-gray-300 text-sm transition-colors"
        >
          <ArrowLeft size={14} />
          Back
        </button>
      </div>

      <div className="w-full max-w-lg">
        {/* Progress steps */}
        <div className="flex items-center gap-2 mb-6">
          {(['upload', 'passphrase', 'importing', 'result'] as Step[]).map((s, i) => (
            <div key={s} className="flex items-center gap-2 flex-1">
              <div className={`w-6 h-6 rounded-full flex items-center justify-center text-xs font-bold flex-shrink-0 transition-all ${
                step === s ? 'bg-vtorrent-500 text-white' :
                ['upload', 'passphrase', 'importing', 'result'].indexOf(step) > i
                  ? 'bg-vtorrent-700 text-vtorrent-300'
                  : 'bg-navy-800 text-gray-600'
              }`}>
                {['upload', 'passphrase', 'importing', 'result'].indexOf(step) > i
                  ? '✓' : i + 1}
              </div>
              {i < 3 && <div className={`h-px flex-1 transition-all ${
                ['upload', 'passphrase', 'importing', 'result'].indexOf(step) > i
                  ? 'bg-vtorrent-700' : 'bg-navy-800'
              }`} />}
            </div>
          ))}
        </div>

        {/* Step: Upload */}
        {step === 'upload' && (
          <div className="card">
            <h2 className="text-lg font-semibold text-white mb-1 flex items-center gap-2">
              <Upload size={18} className="text-amber-400" />
              Select Legacy wallet.dat
            </h2>
            <p className="text-gray-400 text-sm mb-5">
              Select your original vTorrent <code className="text-vtorrent-300 bg-navy-800 px-1 rounded">wallet.dat</code> file.
              All processing happens locally — your keys never leave your device.
            </p>

            {/* Drop zone */}
            <div
              onDrop={handleFileDrop}
              onDragOver={e => e.preventDefault()}
              className={`border-2 border-dashed rounded-xl p-8 text-center transition-all duration-200 cursor-pointer ${
                walletFile
                  ? 'border-vtorrent-500/60 bg-vtorrent-500/5'
                  : 'border-navy-700 hover:border-vtorrent-700/60 hover:bg-navy-800/30'
              }`}
              onClick={() => document.getElementById('wallet-file-input')?.click()}
            >
              <input
                id="wallet-file-input"
                type="file"
                accept=".dat"
                className="hidden"
                onChange={handleFileSelect}
              />
              {walletFile ? (
                <div className="flex flex-col items-center gap-2">
                  <CheckCircle size={32} className="text-vtorrent-400" />
                  <p className="font-medium text-white">{walletFile.name}</p>
                  <p className="text-gray-500 text-sm">{(walletFile.size / 1024).toFixed(1)} KB</p>
                  <p className="text-vtorrent-400 text-xs">Click to change file</p>
                </div>
              ) : (
                <div className="flex flex-col items-center gap-2">
                  <FileText size={32} className="text-gray-600" />
                  <p className="text-gray-400 font-medium">Drop wallet.dat here</p>
                  <p className="text-gray-600 text-sm">or click to browse</p>
                </div>
              )}
            </div>

            {/* Security notice */}
            <div className="mt-4 flex gap-3 bg-amber-900/15 border border-amber-800/30 rounded-lg p-3">
              <Shield size={16} className="text-amber-400 flex-shrink-0 mt-0.5" />
              <p className="text-amber-300/80 text-xs leading-relaxed">
                Your wallet.dat is read entirely offline. No data is transmitted to any server.
                The private keys are decrypted in memory and immediately discarded after import.
              </p>
            </div>

            <button
              disabled={!walletFile}
              onClick={() => setStep('passphrase')}
              className="btn-primary w-full mt-5"
            >
              Continue
              <ArrowRight size={16} className="inline ml-2" />
            </button>
          </div>
        )}

        {/* Step: Passphrase */}
        {step === 'passphrase' && (
          <div className="card">
            <h2 className="text-lg font-semibold text-white mb-1 flex items-center gap-2">
              <Key size={18} className="text-vtorrent-400" />
              Wallet Passphrase
            </h2>
            <p className="text-gray-400 text-sm mb-5">
              If your legacy wallet was encrypted, enter the passphrase you used with the original vTorrent client.
              Leave blank if the wallet was not encrypted.
            </p>

            <div className="space-y-4">
              <div>
                <label className="label">Legacy Wallet Passphrase</label>
                <input
                  type="password"
                  className="input-field"
                  placeholder="Leave blank if wallet was not encrypted"
                  value={passphrase}
                  onChange={e => setPassphrase(e.target.value)}
                  autoFocus
                />
              </div>

              {error && (
                <div className="flex gap-2 bg-red-900/20 border border-red-800/40 rounded-lg p-3">
                  <AlertCircle size={16} className="text-red-400 flex-shrink-0 mt-0.5" />
                  <p className="text-red-300 text-sm">{error}</p>
                </div>
              )}

              <div className="bg-navy-800/60 rounded-lg p-3 text-xs text-gray-500 space-y-1">
                <p className="text-gray-400 font-medium">What about 2FA?</p>
                <p>If your old wallet had 2FA enabled, the migration tool will detect the OTP secret and ask you to confirm with your authenticator app.</p>
              </div>

              <div className="flex gap-3">
                <button onClick={() => setStep('upload')} className="btn-secondary flex-1">
                  Back
                </button>
                <button onClick={handleImport} className="btn-primary flex-1">
                  Import Keys
                  <ArrowRight size={16} className="inline ml-2" />
                </button>
              </div>
            </div>
          </div>
        )}

        {/* Step: Importing */}
        {step === 'importing' && (
          <div className="card text-center py-10">
            <div className="inline-flex items-center justify-center w-16 h-16 rounded-full bg-vtorrent-500/10 border border-vtorrent-500/20 mb-5">
              <div className="w-8 h-8 border-2 border-vtorrent-500 border-t-transparent rounded-full animate-spin" />
            </div>
            <h2 className="text-lg font-semibold text-white mb-2">Parsing wallet.dat</h2>
            <p className="text-gray-400 text-sm">
              Reading BerkeleyDB structure and extracting keys...
            </p>
            <div className="mt-5 space-y-2 text-left max-w-xs mx-auto">
              {[
                'Parsing BerkeleyDB pages...',
                'Decrypting master key...',
                'Extracting private keys...',
                'Deriving legacy addresses...',
                'Checking snapshot balances...',
              ].map((msg, i) => (
                <div key={i} className="flex items-center gap-2 text-xs text-gray-500">
                  <div className="w-1 h-1 rounded-full bg-vtorrent-500 animate-pulse-slow" style={{ animationDelay: `${i * 0.3}s` }} />
                  {msg}
                </div>
              ))}
            </div>
          </div>
        )}

        {/* Step: Result */}
        {step === 'result' && result && (
          <div className="card">
            <div className="text-center mb-6">
              <div className="inline-flex items-center justify-center w-14 h-14 rounded-full bg-emerald-500/10 border border-emerald-500/20 mb-4">
                <CheckCircle size={28} className="text-emerald-400" />
              </div>
              <h2 className="text-lg font-semibold text-white">Import Successful</h2>
              <p className="text-gray-400 text-sm mt-1">Your legacy VTR keys have been imported</p>
            </div>

            {/* Stats */}
            <div className="grid grid-cols-2 gap-3 mb-5">
              <div className="bg-navy-800/60 rounded-lg p-3 text-center">
                <p className="text-2xl font-bold text-vtorrent-300">{result.keysFound}</p>
                <p className="text-xs text-gray-500 mt-0.5">Keys Found</p>
              </div>
              <div className="bg-navy-800/60 rounded-lg p-3 text-center">
                <p className="text-lg font-bold text-emerald-300 font-mono">{formatVTR(result.claimableBalance)}</p>
                <p className="text-xs text-gray-500 mt-0.5">Claimable Balance</p>
              </div>
            </div>

            {/* Badges */}
            <div className="flex flex-wrap gap-2 mb-5">
              {result.hadEncryption && (
                <span className="badge-green"><Shield size={10} /> Encrypted wallet decrypted</span>
              )}
              {result.had2FA && (
                <span className="badge-green"><Shield size={10} /> 2FA verified</span>
              )}
              <span className="badge-green"><Coins size={10} /> Snapshot lookup complete</span>
            </div>

            {/* Addresses */}
            <div className="space-y-2 mb-5">
              <p className="text-xs text-gray-500 font-medium">Imported Addresses</p>
              {result.addresses.map(addr => (
                <div key={addr} className="address-mono text-xs">{addr}</div>
              ))}
            </div>

            <div className="bg-vtorrent-900/20 border border-vtorrent-800/30 rounded-lg p-3 mb-5">
              <p className="text-vtorrent-300 text-xs leading-relaxed">
                <strong>Next step:</strong> Your claimable balance will be available once the new vTorrent 2.0 chain launches.
                You can claim directly from the Dashboard using these imported keys.
              </p>
            </div>

            <button
              onClick={() => navigate('/dashboard')}
              className="btn-primary w-full"
            >
              Go to Dashboard
              <ArrowRight size={16} className="inline ml-2" />
            </button>
          </div>
        )}
      </div>
    </div>
  )
}
