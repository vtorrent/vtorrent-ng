import { useCallback, useEffect, useState, type ReactNode } from 'react'
import {
  AlertCircle,
  CheckCircle,
  Clock,
  RefreshCw,
  ShieldCheck,
  Zap,
} from 'lucide-react'
import { formatVTR, useWallet } from '../hooks/useWallet'
import { useBtcAddress } from '../hooks/useBtc'
import {
  btcClaim,
  btcFund,
  getSwapDeadlines,
  getSwapStatus,
  matchDexOrder,
  reconcileBtcSwap,
  swapRefund,
  vtrClaim,
  type DexOrder,
  type SwapChainStatus,
  type SwapDeadline,
} from '../hooks/useNode'

const VTR_STATE_LABELS: Record<string, string> = {
  not_funded: 'Not funded',
  funding_missing: 'Funding missing',
  funding_prepared: 'Funding prepared',
  funding_pending: 'Funding pending',
  funding_confirming: 'Funding confirming',
  funded: 'Funded',
  invalid_funding: 'Invalid funding',
  claim_prepared: 'Claim prepared',
  claim_pending: 'Claim pending',
  claim_confirming: 'Claim confirming',
  claimed: 'Claimed',
  refund_prepared: 'Refund prepared',
  refund_pending: 'Refund pending',
  refund_confirming: 'Refund confirming',
  refunded: 'Refunded',
  competing_spend_pending: 'Competing spend pending',
  spent_elsewhere: 'Spent elsewhere',
  spend_unknown: 'Spend unknown',
}

const BTC_STATE_LABELS: Record<string, string> = {
  funding_not_observed: 'Not observed',
  invalid_funding: 'Invalid funding',
  funding_immature: 'Funding immature',
  funding_confirming: 'Funding confirming',
  funded: 'Funded',
  claim_confirming: 'Claim confirming',
  claimed: 'Claimed',
  refund_confirming: 'Refund confirming',
  refunded: 'Refunded',
  spent_elsewhere: 'Spent elsewhere',
}

function orderSide(order: DexOrder): 'buy' | 'sell' {
  return order.offerAsset.toUpperCase() === 'VTR' ? 'sell' : 'buy'
}

function formatAsset(satoshis: number, asset: string): string {
  return asset.toUpperCase() === 'VTR'
    ? formatVTR(satoshis)
    : `${(satoshis / 100_000_000).toFixed(8)} ${asset.toUpperCase()}`
}

function relativeTime(seconds: number): string {
  if (seconds <= 0) return 'expired'
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`
  return `${Math.floor(seconds / 86400)}d`
}

function Step({
  label,
  detail,
  state,
}: {
  label: string
  detail?: string
  state: 'done' | 'active' | 'pending' | 'error'
}) {
  const icon = state === 'done'
    ? <CheckCircle size={15} className="text-emerald-400" />
    : state === 'error'
      ? <AlertCircle size={15} className="text-red-400" />
      : state === 'active'
        ? <Clock size={15} className="text-amber-400" />
        : <span className="w-[15px] h-[15px] rounded-full border border-gray-700 block" />
  return (
    <div className="flex items-start gap-3">
      <div className="mt-0.5">{icon}</div>
      <div>
        <p className={`text-sm ${state === 'pending' ? 'text-gray-600' : 'text-gray-200'}`}>{label}</p>
        {detail && <p className="text-xs text-gray-500 mt-0.5">{detail}</p>}
      </div>
    </div>
  )
}

interface SwapWorkspaceProps {
  orders: DexOrder[]
  onRefresh: () => void
}

export default function SwapWorkspace({ orders, onRefresh }: SwapWorkspaceProps) {
  const { defaultAddress } = useWallet()
  const { address: btcAddress, generate: generateBtcAddress } = useBtcAddress()
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [status, setStatus] = useState<SwapChainStatus | null>(null)
  const [deadline, setDeadline] = useState<SwapDeadline | null>(null)
  const [loading, setLoading] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [message, setMessage] = useState('')
  const [passphrase, setPassphrase] = useState('')
  const [preimage, setPreimage] = useState('')
  const [btcRefundAddress, setBtcRefundAddress] = useState('')

  useEffect(() => {
    if (!btcAddress) generateBtcAddress()
  }, [btcAddress, generateBtcAddress])

  const effectiveBtcRefundAddress = btcRefundAddress || btcAddress || ''
  const selected = orders.find(order => order.id === selectedId)
    ?? orders.find(order => order.status === 'matched')
    ?? orders[0]
    ?? null
  const role = selected && defaultAddress === selected.makerAddress ? 'maker' : 'taker'

  const loadStatus = useCallback(async (orderId: string) => {
    await Promise.resolve()
    setLoading(true)
    setError('')
    try {
      const [nextStatus, deadlines] = await Promise.all([
        getSwapStatus(orderId),
        getSwapDeadlines(),
      ])
      setStatus(nextStatus)
      setDeadline(deadlines.find(item => item.orderId === orderId) ?? null)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    const orderId = selected?.id
    if (!orderId) return
    const first = window.setTimeout(() => loadStatus(orderId), 0)
    const timer = window.setInterval(() => loadStatus(orderId), 10_000)
    return () => {
      window.clearTimeout(first)
      window.clearInterval(timer)
    }
  }, [selected?.id, loadStatus])

  const runAction = async (action: () => Promise<unknown>, success: string) => {
    const orderId = selected?.id
    if (!orderId) return
    setBusy(true)
    setError('')
    setMessage('')
    try {
      await action()
      setMessage(success)
      await loadStatus(orderId)
      onRefresh()
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setBusy(false)
    }
  }

  const vtr = status?.vtr
  const btc = status?.btc
  const vtrFunded = Boolean(vtr?.funding)
  const btcFunded = Boolean(btc?.funding)
  const vtrClaimed = vtr?.state === 'claimed' || vtr?.state === 'claim_confirming'
  const btcClaimed = btc?.state === 'claimed' || btc?.state === 'claim_confirming'
  const refunded = Boolean(
    vtr?.state.startsWith('refund') || btc?.state.startsWith('refund')
  )
  const expired = Boolean(deadline && deadline.secondsRemaining <= 0)
  const atRisk = Boolean(deadline?.atRisk)

  let actionTitle = 'Waiting'
  let actionBody = 'Select an order to see the next step.'
  let actionControls: ReactNode = null

  if (selected?.status === 'open') {
    if (role === 'maker') {
      actionTitle = 'Waiting for a taker'
      actionBody = 'Your order is live. Your VTR is locked when a taker matches it.'
    } else {
      actionTitle = 'Match this order'
      actionBody = 'Matching locks your VTR in the HTLC and starts the swap.'
      actionControls = (
        <div className="space-y-3">
          <input
            type="password"
            className="input-field"
            placeholder="Wallet passphrase"
            value={passphrase}
            onChange={event => setPassphrase(event.target.value)}
            disabled={busy}
          />
          <button
            type="button"
            className="btn-primary w-full"
            disabled={!passphrase || !defaultAddress || busy}
            onClick={() => runAction(
              () => matchDexOrder({
                orderId: selected.id,
                takerAddress: defaultAddress ?? '',
                passphrase,
              }),
              'Order matched. Fund the BTC side next.'
            )}
          >
            {busy ? 'Matching…' : 'Match order'}
          </button>
        </div>
      )
    }
  } else if (selected?.status === 'matched') {
    if (refunded) {
      actionTitle = 'Swap refunded'
      actionBody = 'The funded side was returned. No further action is required.'
    } else if (vtrClaimed && btcClaimed) {
      actionTitle = 'Swap complete'
      actionBody = 'Both claims are confirmed. The trade is settled.'
    } else if (!vtrFunded) {
      actionTitle = 'Waiting for VTR funding'
      actionBody = role === 'maker'
        ? 'Your VTR HTLC is being funded and confirmed.'
        : 'The maker is funding the VTR HTLC. This step is automatic.'
    } else if (!btcFunded) {
      if (role === 'taker') {
        actionTitle = 'Fund the BTC side'
        actionBody = 'Send BTC to the HTLC. Your BTC refund address is used if the swap expires.'
        actionControls = (
          <div className="space-y-3">
            <input
              className="input-field font-mono"
              placeholder="BTC refund address"
              value={effectiveBtcRefundAddress}
              onChange={event => setBtcRefundAddress(event.target.value)}
              disabled={busy}
            />
            <button
              type="button"
              className="btn-primary w-full"
              disabled={!effectiveBtcRefundAddress || busy}
              onClick={() => runAction(
                () => btcFund({ orderId: selected.id, btcRefundAddress: effectiveBtcRefundAddress }),
                'BTC funding submitted. Wait for confirmation.'
              )}
            >
              {busy ? 'Funding BTC…' : 'Fund BTC'}
            </button>
          </div>
        )
      } else {
        actionTitle = 'Waiting for BTC funding'
        actionBody = 'The taker must fund the BTC HTLC before the swap can settle.'
      }
    } else if (!vtrClaimed && !btcClaimed) {
      if (role === 'taker') {
        actionTitle = 'Claim VTR'
        actionBody = 'Reveal the swap secret to claim VTR. The maker can then claim BTC.'
        actionControls = (
          <div className="space-y-3">
            <input
              className="input-field font-mono"
              placeholder="Swap secret (preimage, 64 hex chars)"
              value={preimage}
              onChange={event => setPreimage(event.target.value)}
              disabled={busy}
            />
            <input
              type="password"
              className="input-field"
              placeholder="Wallet passphrase"
              value={passphrase}
              onChange={event => setPassphrase(event.target.value)}
              disabled={busy}
            />
            <button
              type="button"
              className="btn-primary w-full"
              disabled={!preimage || !passphrase || busy}
              onClick={() => runAction(
                () => vtrClaim({ orderId: selected.id, preimage, passphrase }),
                'VTR claim submitted.'
              )}
            >
              {busy ? 'Claiming VTR…' : 'Claim VTR'}
            </button>
          </div>
        )
      } else {
        actionTitle = 'Claim BTC'
        actionBody = 'Claim BTC after the taker reveals the secret by claiming VTR.'
        actionControls = (
          <button
            type="button"
            className="btn-primary w-full"
            disabled={busy}
            onClick={() => runAction(
              () => btcClaim(selected.id),
              'BTC claim submitted.'
            )}
          >
            {busy ? 'Claiming BTC…' : 'Claim BTC'}
          </button>
        )
      }
    } else if (vtrClaimed && !btcClaimed) {
      actionTitle = 'Waiting for the BTC claim'
      actionBody = 'The maker must claim BTC to complete the swap.'
    } else if (btcClaimed && !vtrClaimed) {
      actionTitle = 'Waiting for the VTR claim'
      actionBody = 'The taker must claim VTR to complete the swap.'
    }

    if (expired && atRisk && !refunded && !vtrClaimed && !btcClaimed) {
      actionTitle = 'Swap expired'
      actionBody = 'The HTLC deadline passed. Refund the side you funded.'
      actionControls = (
        <div className="flex gap-2">
          <button
            type="button"
            className="flex-1 btn-secondary"
            disabled={busy}
            onClick={() => runAction(
              () => swapRefund(selected.id, 'vtr'),
              'VTR refund submitted.'
            )}
          >
            Refund VTR
          </button>
          <button
            type="button"
            className="flex-1 btn-secondary"
            disabled={busy}
            onClick={() => runAction(
              () => swapRefund(selected.id, 'btc'),
              'BTC refund submitted.'
            )}
          >
            Refund BTC
          </button>
        </div>
      )
    }
  } else if (selected?.status === 'completed') {
    actionTitle = 'Order completed'
    actionBody = 'This order has settled.'
  } else if (selected?.status === 'cancelled') {
    actionTitle = 'Order cancelled'
    actionBody = 'This order was cancelled before settlement.'
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="font-semibold text-white flex items-center gap-2">
            <Zap size={16} className="text-vtorrent-400" />
            Swap workspace
          </h2>
          <p className="text-xs text-gray-500 mt-1">
            Select an order, follow the timeline, and take the next valid action.
          </p>
        </div>
        <button
          type="button"
          onClick={() => selected && loadStatus(selected.id)}
          disabled={!selected || loading}
          className="btn-secondary text-xs flex items-center gap-1.5"
        >
          <RefreshCw size={12} className={loading ? 'animate-spin' : ''} />
          Refresh
        </button>
      </div>

      {orders.length === 0 ? (
        <div className="card text-sm text-gray-500">
          No orders yet. Create an order or wait for the order book to update.
        </div>
      ) : (
        <div className="grid gap-2">
          {orders.map(order => {
            const side = orderSide(order)
            return (
              <button
                key={order.id}
                type="button"
                onClick={() => setSelectedId(order.id)}
                className={`w-full text-left rounded-xl border px-4 py-3 transition-colors ${
                  order.id === selectedId
                    ? 'border-vtorrent-500/60 bg-vtorrent-500/10'
                    : 'border-navy-700/60 bg-navy-900/30 hover:border-navy-600'
                }`}
              >
                <div className="flex items-center justify-between gap-3">
                  <div className="flex items-center gap-2">
                    <span className={`text-xs font-semibold ${side === 'buy' ? 'text-emerald-400' : 'text-red-400'}`}>
                      {side.toUpperCase()}
                    </span>
                    <span className="text-sm text-gray-200">
                      {formatAsset(order.offerAmountSatoshis, order.offerAsset)}
                    </span>
                    <span className="text-xs text-gray-600">for</span>
                    <span className="text-sm text-gray-300">
                      {formatAsset(order.requestAmountSatoshis, order.requestAsset)}
                    </span>
                  </div>
                  <span className={`text-xs px-2 py-0.5 rounded-full ${
                    order.status === 'matched'
                      ? 'bg-vtorrent-900/40 text-vtorrent-300'
                      : order.status === 'open'
                        ? 'bg-emerald-900/30 text-emerald-300'
                        : 'bg-navy-800 text-gray-400'
                  }`}>
                    {order.status}
                  </span>
                </div>
                <div className="flex items-center gap-3 mt-1 text-xs text-gray-600">
                  <span className="font-mono">{order.rate.toFixed(8)}</span>
                  <span>expires {new Date(order.expiresAt * 1000).toLocaleDateString()}</span>
                  <span className="font-mono truncate">{order.id.slice(0, 12)}…</span>
                </div>
              </button>
            )
          })}
        </div>
      )}

      {selected && (
        <div className="card space-y-5">
          <div className="flex items-center justify-between gap-3">
            <div>
              <p className="text-sm font-medium text-white">
                {orderSide(selected) === 'buy' ? 'Buy' : 'Sell'}{' '}
                {formatAsset(selected.offerAmountSatoshis, selected.offerAsset)}
              </p>
              <p className="text-xs text-gray-500 mt-0.5">
                You are the {role}. Order {selected.id.slice(0, 12)}…
              </p>
            </div>
            <span className="text-xs text-gray-500">
              {selected.status === 'matched' ? 'Active swap' : selected.status}
            </span>
          </div>

          {selected.status === 'matched' && (
            <>
              <div className="space-y-3">
                <Step
                  label="VTR locked"
                  detail={vtr ? VTR_STATE_LABELS[vtr.state] ?? vtr.state : 'Loading…'}
                  state={vtrFunded ? 'done' : vtr?.state === 'invalid_funding' ? 'error' : 'active'}
                />
                <Step
                  label="BTC locked"
                  detail={btc ? BTC_STATE_LABELS[btc.state] ?? btc.state : 'BTC state not scanned'}
                  state={btcFunded ? 'done' : btc ? 'active' : 'pending'}
                />
                <Step
                  label="BTC claimed"
                  detail={btcClaimed ? 'Confirmed' : 'Waiting for the maker'}
                  state={btcClaimed ? 'done' : btcFunded ? 'active' : 'pending'}
                />
                <Step
                  label="VTR claimed"
                  detail={vtrClaimed ? 'Confirmed' : 'Waiting for the taker'}
                  state={vtrClaimed ? 'done' : vtrFunded ? 'active' : 'pending'}
                />
              </div>

              {deadline && (
                <div className={`rounded-lg border px-3 py-2.5 text-xs ${
                  expired
                    ? 'border-red-800/40 bg-red-900/15 text-red-300'
                    : atRisk
                      ? 'border-amber-800/40 bg-amber-900/15 text-amber-300'
                      : 'border-navy-700/60 bg-navy-900/40 text-gray-400'
                }`}>
                  BTC HTLC deadline: {relativeTime(deadline.secondsRemaining)}
                  {atRisk ? ' · funds at risk' : ''}
                </div>
              )}

              <div className="rounded-xl border border-vtorrent-900/40 bg-vtorrent-900/10 p-4 space-y-3">
                <div className="flex items-start gap-2">
                  <ShieldCheck size={16} className="text-vtorrent-400 flex-shrink-0 mt-0.5" />
                  <div>
                    <p className="text-sm font-medium text-white">{actionTitle}</p>
                    <p className="text-xs text-gray-400 mt-0.5">{actionBody}</p>
                  </div>
                </div>
                {actionControls}
              </div>
            </>
          )}

          {selected.status === 'open' && actionControls && (
            <div className="rounded-xl border border-vtorrent-900/40 bg-vtorrent-900/10 p-4 space-y-3">
              <div>
                <p className="text-sm font-medium text-white">{actionTitle}</p>
                <p className="text-xs text-gray-400 mt-0.5">{actionBody}</p>
              </div>
              {actionControls}
            </div>
          )}

          {selected.status === 'open' && !actionControls && (
            <div className="rounded-xl border border-navy-700/60 bg-navy-900/40 p-4">
              <p className="text-sm font-medium text-white">{actionTitle}</p>
              <p className="text-xs text-gray-400 mt-0.5">{actionBody}</p>
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

          <div className="flex items-center justify-between gap-3">
            <button
              type="button"
              className="btn-secondary text-xs"
              disabled={busy}
              onClick={() => runAction(
                () => reconcileBtcSwap(selected.id),
                'BTC chain state refreshed.'
              )}
            >
              {busy ? 'Refreshing…' : 'Refresh chain state'}
            </button>
            <details className="text-xs text-gray-500">
              <summary className="cursor-pointer hover:text-gray-300">Advanced details</summary>
              <div className="mt-2 space-y-1 font-mono text-[11px] text-gray-500 break-all">
                <p>order: {selected.id}</p>
                <p>vtr: {vtr?.state ?? 'unknown'}</p>
                <p>btc: {btc?.state ?? 'not scanned'}</p>
                <p>vtr funding: {vtr?.funding?.txid ?? 'none'}</p>
                <p>btc funding: {btc?.funding?.txid ?? 'none'}</p>
                <p>vtr spend: {vtr?.spend?.txid ?? vtr?.pendingSpendTxid ?? 'none'}</p>
                <p>btc spend: {btc?.spend?.txid ?? 'none'}</p>
                <p>deadline: {deadline ? `${deadline.btcExpiry} (${deadline.secondsRemaining}s)` : 'none'}</p>
              </div>
            </details>
          </div>
        </div>
      )}
    </div>
  )
}
