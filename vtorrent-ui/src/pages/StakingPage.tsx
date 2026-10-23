import { useState } from 'react'
import {
  Zap, ZapOff, RefreshCw, TrendingUp, Coins, Snowflake,
  CheckCircle, AlertCircle, Info,
} from 'lucide-react'
import { formatVTR, useWallet } from '../hooks/useWallet'
import { useStakingStatus, startStaking, stopStaking, useNodeInfo, createColdStake, fundColdStake, getTransaction, type ColdStakeKeys } from '../hooks/useNode'
import { isTauri } from '../api'
import { stakingStartError } from '../utils/stakingOps'
import HealthStrip from '../components/staking/HealthStrip'
import ReadinessCard from '../components/staking/ReadinessCard'
import RewardHistory from '../components/staking/RewardHistory'

// ─── Helpers ──────────────────────────────────────────────────────────────────

/** Format a Unix timestamp as a relative "last staked" string. */
function lastStakedAgo(ts: number | null | undefined): string {
  if (!ts) return 'Never'
  const diff = Math.floor(Date.now() / 1000) - ts
  if (diff < 60) return `${diff}s ago`
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`
  return `${Math.floor(diff / 86400)}d ago`
}

/** Format expected daily reward as a human-readable VTR string. */
function formatReward(satoshisPerDay: number): string {
  if (satoshisPerDay <= 0) return '0 VTR/day'
  return `${formatVTR(satoshisPerDay)} / day`
}

function formatDuration(seconds: number): string {
  if (seconds <= 0) return '—'
  if (seconds < 3600) return `${Math.ceil(seconds / 60)}m`
  if (seconds < 86400) return `${Math.ceil(seconds / 3600)}h`
  return `${Math.ceil(seconds / 86400)}d`
}

// ─── Component ────────────────────────────────────────────────────────────────

type ActionStatus = 'idle' | 'loading' | 'success' | 'error'

export default function StakingPage() {
  const { keys, totalBalance } = useWallet()
  const { data: status, loading, error, refresh } = useStakingStatus(8_000)
  const { data: node, loading: nodeLoading, error: nodeError } = useNodeInfo(10_000)

  const [actionStatus, setActionStatus] = useState<ActionStatus>('idle')
  const [actionMsg, setActionMsg] = useState('')
  const [selectedAddress, setSelectedAddress] = useState('')

  const isEnabled = status?.enabled ?? false
  const stakingAddress = status?.stakingAddress ?? null
  const eligibleUtxos = status?.eligibleUtxos ?? 0
  const totalStakingSats = status?.totalStakingSatoshis ?? 0
  const expectedReward = status?.expectedRewardPerDay ?? 0
  const lastStakeTime = status?.lastStakeTime ?? null
  const blocksStaked = status?.blocksStaked ?? 0
  const minStakeAge = status?.minStakeAgeSeconds ?? 6 * 60 * 60
  const maxStakeAge = status?.maxStakeAgeSeconds ?? 6 * 24 * 60 * 60

  // Default to first key if none selected
  const addressOptions = keys.map(k => k.address)
  const effectiveAddress = selectedAddress || addressOptions[0] || ''

  const handleStart = async () => {
    if (!effectiveAddress) {
      setActionMsg('No address available. Please unlock your wallet first.')
      setActionStatus('error')
      return
    }
    setActionStatus('loading')
    setActionMsg('')
    try {
      const result = await startStaking(effectiveAddress)
      setActionStatus('success')
      setActionMsg(result?.warning ?? `Staking started on ${effectiveAddress.slice(0, 12)}…`)
      refresh()
    } catch (e) {
      setActionStatus('error')
      setActionMsg(stakingStartError(e instanceof Error ? e.message : String(e)))
    }
  }

  const handleStop = async () => {
    setActionStatus('loading')
    setActionMsg('')
    try {
      await stopStaking()
      setActionStatus('success')
      setActionMsg('Staking stopped.')
      refresh()
    } catch (e) {
      setActionStatus('error')
      setActionMsg(stakingStartError(e instanceof Error ? e.message : String(e)))
    }
  }

  return (
    <div className="p-6 max-w-3xl mx-auto space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold text-white">Staking</h1>
          <p className="text-sm text-gray-400 mt-0.5">
            Earn VTR rewards by participating in Proof-of-Stake consensus.
          </p>
        </div>
        <button
          onClick={refresh}
          disabled={loading}
          className="p-2 rounded-lg text-gray-400 hover:text-gray-200 hover:bg-navy-800/60 transition-all"
          title="Refresh"
        >
          <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />
        </button>
      </div>

      <HealthStrip info={{ data: node, loading: nodeLoading, error: nodeError }} />

      <ReadinessCard
        status={status}
        node={node}
        totalBalance={totalBalance}
        effectiveAddress={effectiveAddress}
        isEnabled={isEnabled}
      />

      {/* Status banner */}
      {error && (
        <div className="flex items-center gap-2 px-4 py-3 rounded-lg bg-red-900/20 border border-red-800/30 text-red-400 text-sm">
          <AlertCircle size={14} />
          <span>Failed to load staking status: {error}</span>
        </div>
      )}

      {/* Stats grid */}
      <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
        <StatCard
          label="Status"
          value={isEnabled ? 'Active' : 'Inactive'}
          icon={isEnabled ? <Zap size={16} className="text-emerald-400" /> : <ZapOff size={16} className="text-gray-500" />}
          accent={isEnabled ? 'emerald' : 'gray'}
        />
        <StatCard
          label="Staking Balance"
          value={formatVTR(totalStakingSats)}
          icon={<Coins size={16} className="text-vtorrent-400" />}
          accent="vtorrent"
        />
        <StatCard
          label="Estimated Reward"
          value={formatReward(expectedReward)}
          icon={<TrendingUp size={16} className="text-amber-400" />}
          accent="amber"
        />
        <StatCard
          label="Blocks Staked"
          value={blocksStaked.toLocaleString()}
          icon={<CheckCircle size={16} className="text-blue-400" />}
          accent="blue"
        />
      </div>

      {/* Details card */}
      <div className="bg-navy-900/40 border border-vtorrent-900/20 rounded-xl p-5 space-y-4">
        <h2 className="text-sm font-medium text-gray-300">Staking Details</h2>

        <div className="grid grid-cols-2 gap-x-8 gap-y-3 text-sm">
          <DetailRow
            label="Staking Address"
            value={
              <span className="break-all select-all" title={stakingAddress ?? ''}>
                {stakingAddress ?? '—'}
              </span>
            }
          />
          <DetailRow label="Eligible UTXOs" value={eligibleUtxos.toString()} />
          <DetailRow
            label="Avg per UTXO"
            value={eligibleUtxos > 0 ? formatVTR(Math.floor(totalStakingSats / eligibleUtxos)) : '—'}
          />
          <DetailRow label="Last Stake" value={lastStakedAgo(lastStakeTime)} />
          <DetailRow
            label="Maturity Window"
            value={`${formatDuration(minStakeAge)} to ${formatDuration(maxStakeAge)}`}
            hint={`UTXOs must be at least ${formatDuration(minStakeAge)} old and no older than ${formatDuration(maxStakeAge)}.`}
          />
        </div>
      </div>

      <RewardHistory tipHeight={node?.blockHeight ?? null} blocksStaked={blocksStaked} />

      <ColdStakePanel />

      {/* Address selector (only shown when not staking) */}
      {!isEnabled && addressOptions.length > 1 && (
        <div className="space-y-2">
          <label className="text-xs font-medium text-gray-400">Staking Address</label>
          <select
            value={selectedAddress || addressOptions[0]}
            onChange={e => setSelectedAddress(e.target.value)}
            className="w-full bg-navy-900/60 border border-vtorrent-900/30 rounded-lg px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-vtorrent-500/50"
          >
            {addressOptions.map(addr => (
              <option key={addr} value={addr}>{addr}</option>
            ))}
          </select>
        </div>
      )}

      {/* Action feedback */}
      {actionStatus === 'success' && (
        <div className="flex items-center gap-2 px-4 py-3 rounded-lg bg-emerald-900/20 border border-emerald-800/30 text-emerald-400 text-sm">
          <CheckCircle size={14} />
          <span>{actionMsg}</span>
        </div>
      )}
      {actionStatus === 'error' && (
        <div className="flex items-center gap-2 px-4 py-3 rounded-lg bg-red-900/20 border border-red-800/30 text-red-400 text-sm">
          <AlertCircle size={14} />
          <span>{actionMsg}</span>
        </div>
      )}

      {/* Start / Stop button */}
      <div className="flex gap-3">
        {!isEnabled ? (
          <button
            onClick={handleStart}
            disabled={actionStatus === 'loading' || !effectiveAddress}
            className="flex items-center gap-2 px-5 py-2.5 rounded-lg bg-vtorrent-600 hover:bg-vtorrent-500 disabled:opacity-50 disabled:cursor-not-allowed text-white text-sm font-medium transition-all"
          >
            {actionStatus === 'loading' ? (
              <RefreshCw size={14} className="animate-spin" />
            ) : (
              <Zap size={14} />
            )}
            Start Staking
          </button>
        ) : (
          <button
            onClick={handleStop}
            disabled={actionStatus === 'loading'}
            className="flex items-center gap-2 px-5 py-2.5 rounded-lg bg-red-700/80 hover:bg-red-600/80 disabled:opacity-50 disabled:cursor-not-allowed text-white text-sm font-medium transition-all"
          >
            {actionStatus === 'loading' ? (
              <RefreshCw size={14} className="animate-spin" />
            ) : (
              <ZapOff size={14} />
            )}
            Stop Staking
          </button>
        )}
      </div>

      {/* Info note */}
      <div className="flex items-start gap-2 px-4 py-3 rounded-lg bg-navy-900/30 border border-vtorrent-900/20 text-gray-500 text-xs">
        <Info size={12} className="mt-0.5 shrink-0" />
        <span>
          vTorrent uses Proof-of-Stake consensus. Staking requires at least 1 VTR in a
          single UTXO that is at least {formatDuration(minStakeAge)} old and no older
          than {formatDuration(maxStakeAge)}. Rewards are estimates proportional to
          your staking weight relative to the total network stake.
        </span>
      </div>
    </div>
  )
}

// ─── Sub-components ───────────────────────────────────────────────────────────

interface StatCardProps {
  label: string
  value: string
  icon: React.ReactNode
  accent: 'emerald' | 'gray' | 'vtorrent' | 'amber' | 'blue'
}

const accentBg: Record<StatCardProps['accent'], string> = {
  emerald: 'bg-emerald-900/20 border-emerald-800/20',
  gray:    'bg-navy-900/40 border-vtorrent-900/20',
  vtorrent:'bg-vtorrent-900/20 border-vtorrent-800/20',
  amber:   'bg-amber-900/20 border-amber-800/20',
  blue:    'bg-blue-900/20 border-blue-800/20',
}

function StatCard({ label, value, icon, accent }: StatCardProps) {
  return (
    <div className={`rounded-xl border p-4 space-y-2 ${accentBg[accent]}`}>
      <div className="flex items-center gap-2">
        {icon}
        <span className="text-xs text-gray-500">{label}</span>
      </div>
      <p className="text-sm font-medium text-gray-200 font-mono truncate">{value}</p>
    </div>
  )
}

interface DetailRowProps {
  label: string
  value: React.ReactNode
  hint?: string
}

function DetailRow({ label, value, hint }: DetailRowProps) {
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-gray-500 text-xs flex items-center gap-1">
        {label}
        {hint && (
          <span title={hint} className="cursor-help">
            <Info size={10} />
          </span>
        )}
      </span>
      <span className="text-gray-200 font-mono text-xs">{value}</span>
    </div>
  )
}

// ─── Cold staking (P2CS) ───────────────────────────────────────────────────────

function ColdStakePanel() {
  const [role, setRole] = useState<'owner' | 'delegate'>('owner')
  const [keys, setKeys] = useState<ColdStakeKeys | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [amount, setAmount] = useState('')
  const [passphrase, setPassphrase] = useState('')
  const [funding, setFunding] = useState(false)
  const [txid, setTxid] = useState<string | null>(null)
  const [fundedSats, setFundedSats] = useState(0)
  const [savedSpending, setSavedSpending] = useState(false)
  const [copied, setCopied] = useState<string | null>(null)
  const [verifying, setVerifying] = useState(false)
  const [verifyMsg, setVerifyMsg] = useState<string | null>(null)
  const [delegatePackage, setDelegatePackage] = useState('')
  const [delegateBusy, setDelegateBusy] = useState(false)
  const [delegateMsg, setDelegateMsg] = useState<string | null>(null)
  const [delegateError, setDelegateError] = useState<string | null>(null)

  const generate = async () => {
    setBusy(true)
    setError(null)
    setTxid(null)
    setVerifyMsg(null)
    setSavedSpending(false)
    setFundedSats(0)
    try {
      setKeys(await createColdStake(0))
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  const fund = async () => {
    if (!keys) return
    const sats = Math.round(parseFloat(amount) * 100_000_000)
    if (!Number.isFinite(sats) || sats <= 0) {
      setError('Enter a positive VTR amount')
      return
    }
    if (!savedSpending) {
      setError('Save the spending key offline before funding the contract')
      return
    }
    setFunding(true)
    setError(null)
    setTxid(null)
    setVerifyMsg(null)
    try {
      const nextTxid = await fundColdStake(keys.scriptPubkey, sats, passphrase)
      setTxid(nextTxid)
      setFundedSats(sats)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setFunding(false)
    }
  }

  const verify = async () => {
    if (!keys || !txid) return
    setVerifying(true)
    setVerifyMsg(null)
    try {
      const tx = await getTransaction(txid)
      const output = tx.outputs.find(
        item => item.scriptPubkey === keys.scriptPubkey && item.valueSatoshis === fundedSats
      )
      if (!output) {
        setVerifyMsg('The transaction does not contain the expected P2CS output yet.')
      } else if (tx.blockHeight === null) {
        setVerifyMsg('Funding is in the mempool. Verify again after confirmation.')
      } else {
        setVerifyMsg(
          `Verified: ${formatVTR(output.valueSatoshis)} locked to the P2CS script at block ${tx.blockHeight}.`
        )
      }
    } catch (e) {
      setVerifyMsg(e instanceof Error ? e.message : String(e))
    } finally {
      setVerifying(false)
    }
  }

  const copy = async (text: string, label: string) => {
    try {
      await navigator.clipboard.writeText(text)
      setCopied(label)
      window.setTimeout(() => setCopied(null), 1500)
    } catch {
      setError('Clipboard is unavailable. Select and copy the text manually.')
    }
  }

  const stakingPackage = keys
    ? JSON.stringify({
        version: 1,
        kind: 'vtorrent-cold-stake-staking',
        staking_address: keys.stakingAddress,
        script_pubkey: keys.scriptPubkey,
        staking_wif: keys.stakingWif,
        locktime: keys.locktime,
      }, null, 2)
    : ''
  const spendingPackage = keys
    ? JSON.stringify({
        version: 1,
        kind: 'vtorrent-cold-stake-spending',
        spending_address: keys.spendingAddress,
        script_pubkey: keys.scriptPubkey,
        spending_wif: keys.spendingWif,
        locktime: keys.locktime,
      }, null, 2)
    : ''

  const startDelegate = async () => {
    setDelegateBusy(true)
    setDelegateError(null)
    setDelegateMsg(null)
    try {
      const parsed = JSON.parse(delegatePackage) as {
        kind?: string
        staking_address?: string
        staking_wif?: string
      }
      if (
        parsed.kind !== 'vtorrent-cold-stake-staking'
        || !parsed.staking_address
        || !parsed.staking_wif
      ) {
        throw new Error('Paste a valid vTorrent cold-stake staking package')
      }
      await startStaking(parsed.staking_address, parsed.staking_wif)
      setDelegateMsg('Staking started with the imported staking key.')
    } catch (e) {
      setDelegateError(e instanceof Error ? e.message : String(e))
    } finally {
      setDelegateBusy(false)
    }
  }

  return (
    <div className="card">
      <div className="flex items-center justify-between mb-2">
        <h2 className="font-semibold text-white text-sm flex items-center gap-2">
          <Snowflake size={14} className="text-sky-400" /> Cold staking
        </h2>
        {role === 'owner' && (
          <button
            data-testid="cold-stake-generate"
            onClick={generate}
            disabled={busy}
            className="btn-secondary text-xs"
          >
            {busy ? 'Generating…' : 'Generate key pair'}
          </button>
        )}
      </div>

      <div className="flex gap-1 bg-navy-800/60 rounded-lg p-1 mb-3">
        <button
          onClick={() => setRole('owner')}
          className={`flex-1 py-1.5 rounded-md text-xs font-medium transition-all ${
            role === 'owner' ? 'bg-vtorrent-500/15 text-vtorrent-300' : 'text-gray-500 hover:text-gray-300'
          }`}
        >
          Fund a contract
        </button>
        <button
          onClick={() => setRole('delegate')}
          className={`flex-1 py-1.5 rounded-md text-xs font-medium transition-all ${
            role === 'delegate' ? 'bg-vtorrent-500/15 text-vtorrent-300' : 'text-gray-500 hover:text-gray-300'
          }`}
        >
          Run a staking node
        </button>
      </div>

      {role === 'owner' ? (
        <>
          <p className="text-xs text-gray-500 mb-3">
            Stake from a node that holds only the <strong>staking key</strong>. The
            <strong> spending key</strong> stays offline and is the only key that can move funds.
          </p>
          {error && <p className="text-xs text-red-400">{error}</p>}
          {keys && (
            <div className="space-y-3 text-xs">
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <div className="text-gray-500">Staking address (hot)</div>
                  <div className="font-mono text-amber-300 break-all">{keys.stakingAddress}</div>
                </div>
                <div>
                  <div className="text-gray-500">Spending address (cold)</div>
                  <div className="font-mono text-red-300 break-all">{keys.spendingAddress}</div>
                </div>
              </div>
              <div>
                <div className="text-gray-500">P2CS script (fund this)</div>
                <div data-testid="cold-stake-script" className="font-mono text-vtorrent-300 break-all">{keys.scriptPubkey}</div>
              </div>
              <div>
                <div className="text-gray-500">Script hash</div>
                <div data-testid="cold-stake-script-hash" className="font-mono text-gray-400 break-all">{keys.scriptHash}</div>
              </div>
              <div>
                <div className="flex items-center justify-between gap-2">
                  <div className="text-gray-500">Staking key package (hot — give to the staking node)</div>
                  <button
                    data-testid="cold-stake-copy-staking"
                    onClick={() => copy(stakingPackage, 'staking')}
                    className="text-vtorrent-400 hover:text-vtorrent-300"
                  >
                    {copied === 'staking' ? 'Copied' : 'Copy package'}
                  </button>
                </div>
                <div data-testid="cold-stake-staking-wif" className="font-mono text-amber-300 break-all">{keys.stakingWif}</div>
              </div>
              <div>
                <div className="flex items-center justify-between gap-2">
                  <div className="text-gray-500">Spending key (cold — keep offline, shown once)</div>
                  <button
                    data-testid="cold-stake-copy-spending"
                    onClick={() => copy(spendingPackage, 'spending')}
                    className="text-red-400 hover:text-red-300"
                  >
                    {copied === 'spending' ? 'Copied' : 'Copy key'}
                  </button>
                </div>
                <div data-testid="cold-stake-spending-wif" className="font-mono text-red-300 break-all">{keys.spendingWif}</div>
              </div>
              <p className="text-red-400">
                The spending key is not stored by this node. Save it offline now.
              </p>
              <label className="flex items-start gap-2 text-gray-400">
                <input
                  data-testid="cold-stake-saved-spending"
                  type="checkbox"
                  className="mt-0.5"
                  checked={savedSpending}
                  onChange={event => setSavedSpending(event.target.checked)}
                />
                I saved the spending key offline and understand it is shown only once.
              </label>
              <div className="flex flex-wrap items-center gap-2 pt-1">
                <input
                  data-testid="cold-stake-amount"
                  value={amount}
                  onChange={e => setAmount(e.target.value)}
                  placeholder="Amount (VTR)"
                  className="input-field text-xs"
                />
                {!isTauri() && (
                  <input
                    data-testid="cold-stake-passphrase"
                    type="password"
                    value={passphrase}
                    onChange={e => setPassphrase(e.target.value)}
                    placeholder="Wallet passphrase"
                    className="input-field text-xs"
                  />
                )}
                <button
                  data-testid="cold-stake-fund"
                  onClick={fund}
                  disabled={funding || !savedSpending}
                  className="btn-secondary text-xs"
                >
                  {funding ? 'Funding…' : 'Fund cold stake'}
                </button>
              </div>
              {txid && (
                <div className="space-y-2">
                  <p data-testid="cold-stake-txid" className="text-emerald-400 break-all">Funding txid: {txid}</p>
                  <button
                    data-testid="cold-stake-verify"
                    onClick={verify}
                    disabled={verifying}
                    className="btn-secondary text-xs"
                  >
                    {verifying ? 'Verifying…' : 'Verify contract'}
                  </button>
                  {verifyMsg && (
                    <p data-testid="cold-stake-verify-msg" className="text-gray-400">{verifyMsg}</p>
                  )}
                </div>
              )}
              <p className="text-gray-500">
                The old P2SH label is not stakeable. Fund the raw script above; do not
                send to the label with a generic send.
              </p>
            </div>
          )}
        </>
      ) : (
        <div className="space-y-3 text-xs">
          <p className="text-gray-500">
            Paste the staking package from the contract owner. The node uses the
            staking key to sign coinstakes. Never paste the spending key here.
          </p>
          <textarea
            data-testid="cold-stake-delegate-package"
            value={delegatePackage}
            onChange={event => setDelegatePackage(event.target.value)}
            placeholder='{"kind":"vtorrent-cold-stake-staking", ...}'
            className="input-field font-mono text-xs h-28 resize-none"
          />
          {delegateError && <p className="text-red-400">{delegateError}</p>}
          {delegateMsg && <p data-testid="cold-stake-delegate-msg" className="text-emerald-400">{delegateMsg}</p>}
          <button
            data-testid="cold-stake-delegate-start"
            onClick={startDelegate}
            disabled={delegateBusy || !delegatePackage.trim()}
            className="btn-secondary text-xs"
          >
            {delegateBusy ? 'Starting…' : 'Start staking with this key'}
          </button>
          <p className="text-gray-500">
            The staking key is hot. Keep the spending key offline; it is the only key
            that can move the delegated funds.
          </p>
        </div>
      )}
    </div>
  )
}
