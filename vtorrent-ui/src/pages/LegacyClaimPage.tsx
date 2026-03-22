import { useState } from 'react'
import {
  Gift, Search, ArrowRight, CheckCircle, AlertCircle,
  RefreshCw, Key, Wallet, Info, ExternalLink,
} from 'lucide-react'
import { formatVTR, useWallet } from '../hooks/useWallet'
import { checkLegacyClaim, submitLegacyClaim } from '../hooks/useNode'

// ─── Types ────────────────────────────────────────────────────────────────────

type Step = 'check' | 'confirm' | 'done'
type Status = 'idle' | 'loading' | 'success' | 'error'

interface ClaimInfo {
  address: string
  claimableSatoshis: number
  display: string
  alreadyClaimed: boolean
}

// ─── Component ────────────────────────────────────────────────────────────────

export default function LegacyClaimPage() {
  const { keys } = useWallet()

  const [step, setStep] = useState<Step>('check')
  const [status, setStatus] = useState<Status>('idle')
  const [errorMsg, setErrorMsg] = useState('')

  // Step 1: check
  const [legacyAddress, setLegacyAddress] = useState('')
  const [claimInfo, setClaimInfo] = useState<ClaimInfo | null>(null)

  // Step 2: confirm
  const [wifKey, setWifKey] = useState('')
  const [recipientAddress, setRecipientAddress] = useState(keys[0]?.address ?? '')

  // Step 3: done
  const [txid, setTxid] = useState('')
  const [claimedSats, setClaimedSats] = useState(0)

  // ── Step 1: Check balance ──────────────────────────────────────────────────

  const handleCheck = async () => {
    if (!legacyAddress.trim()) return
    setStatus('loading')
    setErrorMsg('')
    setClaimInfo(null)
    try {
      const info = await checkLegacyClaim(legacyAddress.trim())
      setClaimInfo(info)
      setStatus('idle')
    } catch (e) {
      setErrorMsg(e instanceof Error ? e.message : String(e))
      setStatus('error')
    }
  }

  // ── Step 2: Submit claim ───────────────────────────────────────────────────

  const handleSubmit = async () => {
    if (!wifKey.trim() || !recipientAddress.trim()) return
    setStatus('loading')
    setErrorMsg('')
    try {
      const result = await submitLegacyClaim(wifKey.trim(), recipientAddress.trim())
      setTxid(result.txid)
      setClaimedSats(result.claimedSatoshis)
      setStep('done')
      setStatus('idle')
    } catch (e) {
      setErrorMsg(e instanceof Error ? e.message : String(e))
      setStatus('error')
    }
  }

  // ── Reset ──────────────────────────────────────────────────────────────────

  const handleReset = () => {
    setStep('check')
    setStatus('idle')
    setErrorMsg('')
    setLegacyAddress('')
    setClaimInfo(null)
    setWifKey('')
    setTxid('')
    setClaimedSats(0)
  }

  return (
    <div className="p-6 max-w-2xl mx-auto space-y-6">
      {/* Header */}
      <div>
        <h1 className="text-xl font-semibold text-white flex items-center gap-2">
          <Gift size={20} className="text-vtorrent-400" />
          Legacy VTR Claim
        </h1>
        <p className="text-sm text-gray-400 mt-1">
          If you held VTR before the chain migration, you can claim your balance here.
          Your legacy private key is used to prove ownership — it is never transmitted.
        </p>
      </div>

      {/* Step indicator */}
      <StepIndicator current={step} />

      {/* Error banner */}
      {status === 'error' && (
        <div className="flex items-center gap-2 px-4 py-3 rounded-lg bg-red-900/20 border border-red-800/30 text-red-400 text-sm">
          <AlertCircle size={14} />
          <span>{errorMsg}</span>
        </div>
      )}

      {/* ── Step 1: Check ── */}
      {step === 'check' && (
        <div className="bg-navy-900/40 border border-vtorrent-900/20 rounded-xl p-5 space-y-4">
          <h2 className="text-sm font-medium text-gray-300 flex items-center gap-2">
            <Search size={14} />
            Check Legacy Balance
          </h2>

          <div className="space-y-2">
            <label className="text-xs text-gray-400">Legacy VTR Address</label>
            <input
              type="text"
              placeholder="V…"
              value={legacyAddress}
              onChange={e => setLegacyAddress(e.target.value)}
              className="w-full bg-navy-900/60 border border-vtorrent-900/30 rounded-lg px-3 py-2 text-sm text-gray-200 placeholder-gray-600 focus:outline-none focus:border-vtorrent-500/50 font-mono"
            />
            <p className="text-xs text-gray-600">
              Enter the legacy address that held VTR before the migration snapshot.
            </p>
          </div>

          <button
            onClick={handleCheck}
            disabled={!legacyAddress.trim() || status === 'loading'}
            className="flex items-center gap-2 px-4 py-2 rounded-lg bg-vtorrent-600 hover:bg-vtorrent-500 disabled:opacity-50 disabled:cursor-not-allowed text-white text-sm font-medium transition-all"
          >
            {status === 'loading' ? (
              <RefreshCw size={14} className="animate-spin" />
            ) : (
              <Search size={14} />
            )}
            Check Balance
          </button>

          {/* Result */}
          {claimInfo && (
            <div className="mt-2 p-4 rounded-lg bg-navy-900/60 border border-vtorrent-900/30 space-y-3">
              {claimInfo.alreadyClaimed ? (
                <div className="flex items-center gap-2 text-amber-400 text-sm">
                  <AlertCircle size={14} />
                  <span>This address has already been claimed.</span>
                </div>
              ) : claimInfo.claimableSatoshis === 0 ? (
                <div className="flex items-center gap-2 text-gray-500 text-sm">
                  <Info size={14} />
                  <span>No claimable balance found for this address.</span>
                </div>
              ) : (
                <>
                  <div className="flex items-center gap-2 text-emerald-400 text-sm">
                    <CheckCircle size={14} />
                    <span>Claimable balance found!</span>
                  </div>
                  <div className="flex items-center justify-between">
                    <span className="text-xs text-gray-500">Amount</span>
                    <span className="text-sm font-mono text-vtorrent-300 font-medium">
                      {formatVTR(claimInfo.claimableSatoshis)}
                    </span>
                  </div>
                  <div className="flex items-center justify-between">
                    <span className="text-xs text-gray-500">Address</span>
                    <span className="text-xs font-mono text-gray-400 truncate max-w-[200px]">
                      {claimInfo.address}
                    </span>
                  </div>
                  <button
                    onClick={() => setStep('confirm')}
                    className="w-full flex items-center justify-center gap-2 px-4 py-2 rounded-lg bg-vtorrent-600 hover:bg-vtorrent-500 text-white text-sm font-medium transition-all"
                  >
                    Proceed to Claim
                    <ArrowRight size={14} />
                  </button>
                </>
              )}
            </div>
          )}
        </div>
      )}

      {/* ── Step 2: Confirm ── */}
      {step === 'confirm' && claimInfo && (
        <div className="bg-navy-900/40 border border-vtorrent-900/20 rounded-xl p-5 space-y-4">
          <h2 className="text-sm font-medium text-gray-300 flex items-center gap-2">
            <Key size={14} />
            Confirm &amp; Sign Claim
          </h2>

          {/* Summary */}
          <div className="p-3 rounded-lg bg-vtorrent-900/20 border border-vtorrent-800/20 text-sm space-y-1">
            <div className="flex justify-between">
              <span className="text-gray-500">Claiming</span>
              <span className="font-mono text-vtorrent-300">{formatVTR(claimInfo.claimableSatoshis)}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-gray-500">From</span>
              <span className="font-mono text-gray-400 text-xs">{claimInfo.address.slice(0, 20)}…</span>
            </div>
          </div>

          {/* WIF key input */}
          <div className="space-y-2">
            <label className="text-xs text-gray-400">Legacy WIF Private Key</label>
            <input
              type="password"
              placeholder="5… or K… or L…"
              value={wifKey}
              onChange={e => setWifKey(e.target.value)}
              className="w-full bg-navy-900/60 border border-vtorrent-900/30 rounded-lg px-3 py-2 text-sm text-gray-200 placeholder-gray-600 focus:outline-none focus:border-vtorrent-500/50 font-mono"
            />
            <p className="text-xs text-gray-600">
              Your WIF key is used locally to sign the claim transaction. It is never sent to any server.
            </p>
          </div>

          {/* Recipient address */}
          <div className="space-y-2">
            <label className="text-xs text-gray-400">Recipient Address (new chain)</label>
            {keys.length > 0 ? (
              <select
                value={recipientAddress}
                onChange={e => setRecipientAddress(e.target.value)}
                className="w-full bg-navy-900/60 border border-vtorrent-900/30 rounded-lg px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-vtorrent-500/50"
              >
                {keys.map(k => (
                  <option key={k.address} value={k.address}>{k.address}</option>
                ))}
              </select>
            ) : (
              <input
                type="text"
                placeholder="V…"
                value={recipientAddress}
                onChange={e => setRecipientAddress(e.target.value)}
                className="w-full bg-navy-900/60 border border-vtorrent-900/30 rounded-lg px-3 py-2 text-sm text-gray-200 placeholder-gray-600 focus:outline-none focus:border-vtorrent-500/50 font-mono"
              />
            )}
          </div>

          {/* Security note */}
          <div className="flex items-start gap-2 px-3 py-2 rounded-lg bg-amber-900/10 border border-amber-800/20 text-amber-500 text-xs">
            <Info size={11} className="mt-0.5 shrink-0" />
            <span>
              After claiming, transfer any remaining funds from your legacy address to a new wallet.
              Do not reuse legacy keys.
            </span>
          </div>

          <div className="flex gap-3">
            <button
              onClick={() => { setStep('check'); setErrorMsg(''); setStatus('idle') }}
              className="px-4 py-2 rounded-lg border border-vtorrent-900/30 text-gray-400 hover:text-gray-200 text-sm transition-all"
            >
              Back
            </button>
            <button
              onClick={handleSubmit}
              disabled={!wifKey.trim() || !recipientAddress.trim() || status === 'loading'}
              className="flex items-center gap-2 px-5 py-2 rounded-lg bg-vtorrent-600 hover:bg-vtorrent-500 disabled:opacity-50 disabled:cursor-not-allowed text-white text-sm font-medium transition-all"
            >
              {status === 'loading' ? (
                <RefreshCw size={14} className="animate-spin" />
              ) : (
                <Wallet size={14} />
              )}
              Submit Claim
            </button>
          </div>
        </div>
      )}

      {/* ── Step 3: Done ── */}
      {step === 'done' && (
        <div className="bg-navy-900/40 border border-emerald-800/20 rounded-xl p-5 space-y-4 text-center">
          <div className="flex justify-center">
            <div className="w-14 h-14 rounded-full bg-emerald-900/30 border border-emerald-700/30 flex items-center justify-center">
              <CheckCircle size={28} className="text-emerald-400" />
            </div>
          </div>
          <div>
            <h2 className="text-base font-semibold text-white">Claim Submitted!</h2>
            <p className="text-sm text-gray-400 mt-1">
              Your claim transaction has been broadcast to the network.
            </p>
          </div>

          <div className="p-3 rounded-lg bg-navy-900/60 border border-vtorrent-900/30 text-left space-y-2">
            <div className="flex justify-between text-sm">
              <span className="text-gray-500">Amount Claimed</span>
              <span className="font-mono text-vtorrent-300 font-medium">{formatVTR(claimedSats)}</span>
            </div>
            <div className="flex justify-between text-sm">
              <span className="text-gray-500">Transaction ID</span>
              <span className="font-mono text-gray-400 text-xs truncate max-w-[200px]">{txid}</span>
            </div>
          </div>

          <p className="text-xs text-gray-600">
            Your VTR will appear in your wallet after the transaction is confirmed (typically within 1–2 blocks).
          </p>

          <button
            onClick={handleReset}
            className="flex items-center gap-2 mx-auto px-4 py-2 rounded-lg border border-vtorrent-900/30 text-gray-400 hover:text-gray-200 text-sm transition-all"
          >
            <Gift size={14} />
            Claim Another Address
          </button>
        </div>
      )}

      {/* Info footer */}
      <div className="flex items-start gap-2 px-4 py-3 rounded-lg bg-navy-900/30 border border-vtorrent-900/20 text-gray-500 text-xs">
        <Info size={12} className="mt-0.5 shrink-0" />
        <span>
          The legacy snapshot was taken at block 1,200,000 of the original vTorrent chain.
          Unclaimed balances expire after 2 years from the migration date.
          If you need help, visit the{' '}
          <a
            href="https://vtorrent.io/claim"
            target="_blank"
            rel="noopener noreferrer"
            className="text-vtorrent-400 hover:underline inline-flex items-center gap-0.5"
          >
            claim guide <ExternalLink size={10} />
          </a>.
        </span>
      </div>
    </div>
  )
}

// ─── Step Indicator ───────────────────────────────────────────────────────────

interface StepIndicatorProps {
  current: Step
}

const STEPS: { id: Step; label: string }[] = [
  { id: 'check',   label: 'Check Balance' },
  { id: 'confirm', label: 'Sign & Submit' },
  { id: 'done',    label: 'Complete'      },
]

function StepIndicator({ current }: StepIndicatorProps) {
  const currentIdx = STEPS.findIndex(s => s.id === current)
  return (
    <div className="flex items-center gap-2">
      {STEPS.map((s, i) => (
        <div key={s.id} className="flex items-center gap-2">
          <div className={`flex items-center gap-1.5 text-xs font-medium ${
            i < currentIdx
              ? 'text-emerald-400'
              : i === currentIdx
              ? 'text-vtorrent-300'
              : 'text-gray-600'
          }`}>
            <span className={`w-5 h-5 rounded-full flex items-center justify-center text-xs border ${
              i < currentIdx
                ? 'bg-emerald-900/30 border-emerald-700/40 text-emerald-400'
                : i === currentIdx
                ? 'bg-vtorrent-900/30 border-vtorrent-600/40 text-vtorrent-300'
                : 'bg-navy-900/40 border-vtorrent-900/20 text-gray-600'
            }`}>
              {i < currentIdx ? <CheckCircle size={10} /> : i + 1}
            </span>
            {s.label}
          </div>
          {i < STEPS.length - 1 && (
            <div className={`h-px w-8 ${i < currentIdx ? 'bg-emerald-700/40' : 'bg-vtorrent-900/20'}`} />
          )}
        </div>
      ))}
    </div>
  )
}
