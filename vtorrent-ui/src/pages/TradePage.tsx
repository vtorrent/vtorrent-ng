import { useState } from 'react'
import {
  Plus, CheckCircle, XCircle,
  Zap, Info, RefreshCw, AlertCircle, ArrowRightLeft
} from 'lucide-react'
import { formatVTR } from '../hooks/useWallet'
import {
  useDexOrders, cancelDexOrder, matchDexOrder, btcFund, vtrClaim, btcClaim, swapRefund,
  type DexOrder,
} from '../hooks/useNode'

// ─── Helpers ──────────────────────────────────────────────────────────────────

/** Convert a Unix timestamp to a relative expiry string like "23h" or "expired". */
function expiresIn(expiresAt: number): string {
  const diff = expiresAt - Math.floor(Date.now() / 1000)
  if (diff <= 0) return 'expired'
  if (diff < 3600) return `${Math.floor(diff / 60)}m`
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`
  return `${Math.floor(diff / 86400)}d`
}

/** Convert a Unix timestamp to a relative "created" string like "5m ago". */
function createdAgo(createdAt: number): string {
  const diff = Math.floor(Date.now() / 1000) - createdAt
  if (diff < 60) return `${diff}s ago`
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`
  return `${Math.floor(diff / 86400)}d ago`
}

/** Derive order side from offer/request assets. */
function orderSide(order: DexOrder): 'buy' | 'sell' {
  // If the maker is offering BTC and requesting VTR → buy order (from VTR perspective)
  // If the maker is offering VTR and requesting BTC → sell order
  return order.offerAsset.toUpperCase() === 'VTR' ? 'sell' : 'buy'
}

// ─── Component ────────────────────────────────────────────────────────────────

export default function TradePage() {
  const { data: orders, loading, error, refresh } = useDexOrders(10_000)
  const [tab, setTab] = useState<'orderbook' | 'create' | 'myorders' | 'swap'>('orderbook')
  const [orderType, setOrderType] = useState<'buy' | 'sell'>('buy')
  const [amount, setAmount] = useState('')
  const [price, setPrice] = useState('')
  const [cancelling, setCancelling] = useState<string | null>(null)

  // Swap lifecycle state
  const [swapOrderId, setSwapOrderId] = useState('')
  const [takerAddress, setTakerAddress] = useState('')
  const [btcRefundAddress, setBtcRefundAddress] = useState('')
  const [preimage, setPreimage] = useState('')
  const [swapBusy, setSwapBusy] = useState(false)
  const [swapResult, setSwapResult] = useState<string | null>(null)
  const [swapError, setSwapError] = useState<string | null>(null)

  const total = amount && price ? (parseFloat(amount) * parseFloat(price)).toFixed(8) : '0'

  // Separate open orders into sides
  const openOrders = orders.filter(o => o.status === 'open')
  const sells = openOrders.filter(o => orderSide(o) === 'sell').sort((a, b) => a.rate - b.rate)
  const buys  = openOrders.filter(o => orderSide(o) === 'buy').sort((a, b) => b.rate - a.rate)
  const spread = sells.length && buys.length
    ? ((sells[0].rate - buys[0].rate) / sells[0].rate * 100).toFixed(2)
    : null

  const handleCancel = async (id: string) => {
    setCancelling(id)
    try {
      await cancelDexOrder(id)
      refresh()
    } catch {
      // best-effort
    } finally {
      setCancelling(null)
    }
  }

  const runSwap = async (action: () => Promise<unknown>, success: string) => {
    setSwapBusy(true)
    setSwapError(null)
    setSwapResult(null)
    try {
      await action()
      setSwapResult(success)
      refresh()
    } catch (e) {
      setSwapError(e instanceof Error ? e.message : String(e))
    } finally {
      setSwapBusy(false)
    }
  }

  return (
    <div className="p-6 space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-bold text-white">P2P Trade</h1>
          <p className="text-gray-500 text-sm mt-0.5">Trade VTR directly with other users via atomic swaps. No exchange. No custodian.</p>
        </div>
        <button
          onClick={refresh}
          className="p-2 rounded-lg hover:bg-navy-800 text-gray-500 hover:text-gray-300 transition-colors"
          title="Refresh order book"
        >
          <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
        </button>
      </div>

      {/* How it works banner */}
      <div className="bg-vtorrent-900/20 border border-vtorrent-800/30 rounded-xl p-4">
        <div className="flex items-start gap-3">
          <Zap size={18} className="text-vtorrent-400 flex-shrink-0 mt-0.5" />
          <div>
            <p className="text-vtorrent-300 font-medium text-sm mb-1">Atomic Swap Trading — No Exchange Required</p>
            <p className="text-gray-400 text-xs leading-relaxed">
              All trades use <strong className="text-gray-300">Hash Time-Locked Contracts (HTLCs)</strong> — a cryptographic protocol where both parties either complete the swap simultaneously or neither does.
              Your VTR never leaves your wallet until the counterparty's BTC (or other asset) is confirmed.
              No sign-up. No KYC. No withdrawal limits.
            </p>
          </div>
        </div>
      </div>

      {/* Tabs */}
      <div className="flex gap-1 bg-navy-900/60 rounded-xl p-1 w-fit">
        {(['orderbook', 'create', 'myorders', 'swap'] as const).map(t => (
          <button
            key={t}
            onClick={() => setTab(t)}
            className={`px-4 py-2 rounded-lg text-sm font-medium transition-all ${
              tab === t
                ? 'bg-vtorrent-500/15 text-vtorrent-300 border border-vtorrent-500/20'
                : 'text-gray-500 hover:text-gray-300'
            }`}
          >
            {t === 'orderbook' ? 'Order Book' : t === 'create' ? 'Create Order' : t === 'myorders' ? 'My Orders' : 'Swap'}
          </button>
        ))}
      </div>

      {/* Order Book — live data */}
      {tab === 'orderbook' && (
        <>
          {error ? (
            <div className="flex items-center gap-2 text-xs text-red-400 py-4">
              <AlertCircle size={13} />
              Could not load order book — node may be offline.
            </div>
          ) : loading && orders.length === 0 ? (
            <div className="flex items-center gap-2 text-xs text-gray-600 py-4">
              <RefreshCw size={12} className="animate-spin" />
              Loading order book…
            </div>
          ) : (
            <div className="grid grid-cols-2 gap-4">
              {/* Sell orders (asks) */}
              <div className="card">
                <h3 className="text-sm font-semibold text-red-400 mb-3">Sell Orders (Asks)</h3>
                <div className="space-y-1">
                  <div className="grid grid-cols-3 text-xs text-gray-600 pb-2 border-b border-navy-800">
                    <span>Rate</span>
                    <span className="text-right">Amount (VTR)</span>
                    <span className="text-right">Expires</span>
                  </div>
                  {sells.length === 0 ? (
                    <p className="text-xs text-gray-600 py-3">No sell orders.</p>
                  ) : sells.map((order: DexOrder) => (
                    <div key={order.id} className="grid grid-cols-3 text-xs py-1.5 hover:bg-red-900/10 rounded px-1 cursor-pointer transition-colors">
                      <span className="text-red-400 font-mono">{order.rate.toFixed(8)}</span>
                      <span className="text-right text-gray-300 font-mono">{formatVTR(order.offerAmountSatoshis)}</span>
                      <span className="text-right text-gray-600">{expiresIn(order.expiresAt)}</span>
                    </div>
                  ))}
                </div>
              </div>

              {/* Buy orders (bids) */}
              <div className="card">
                <h3 className="text-sm font-semibold text-emerald-400 mb-3">Buy Orders (Bids)</h3>
                <div className="space-y-1">
                  <div className="grid grid-cols-3 text-xs text-gray-600 pb-2 border-b border-navy-800">
                    <span>Rate</span>
                    <span className="text-right">Amount (VTR)</span>
                    <span className="text-right">Expires</span>
                  </div>
                  {buys.length === 0 ? (
                    <p className="text-xs text-gray-600 py-3">No buy orders.</p>
                  ) : buys.map((order: DexOrder) => (
                    <div key={order.id} className="grid grid-cols-3 text-xs py-1.5 hover:bg-emerald-900/10 rounded px-1 cursor-pointer transition-colors">
                      <span className="text-emerald-400 font-mono">{order.rate.toFixed(8)}</span>
                      <span className="text-right text-gray-300 font-mono">{formatVTR(order.requestAmountSatoshis)}</span>
                      <span className="text-right text-gray-600">{expiresIn(order.expiresAt)}</span>
                    </div>
                  ))}
                </div>
              </div>

              {spread && (
                <div className="col-span-2 text-center text-xs text-gray-500">
                  Spread: <span className="text-vtorrent-400 font-mono">{spread}%</span>
                  &nbsp;·&nbsp;
                  Best ask: <span className="text-red-400 font-mono">{sells[0]?.rate.toFixed(8)}</span>
                  &nbsp;·&nbsp;
                  Best bid: <span className="text-emerald-400 font-mono">{buys[0]?.rate.toFixed(8)}</span>
                </div>
              )}
            </div>
          )}
        </>
      )}

      {/* Create Order */}
      {tab === 'create' && (
        <div className="max-w-md card">
          <h3 className="font-semibold text-white text-sm mb-4 flex items-center gap-2">
            <Plus size={16} className="text-vtorrent-400" />
            Create Atomic Swap Order
          </h3>

          {/* Buy/Sell toggle */}
          <div className="flex gap-1 bg-navy-800/60 rounded-lg p-1 mb-4">
            <button
              onClick={() => setOrderType('buy')}
              className={`flex-1 py-2 rounded-md text-sm font-medium transition-all ${
                orderType === 'buy' ? 'bg-emerald-600 text-white' : 'text-gray-500 hover:text-gray-300'
              }`}
            >
              Buy VTR
            </button>
            <button
              onClick={() => setOrderType('sell')}
              className={`flex-1 py-2 rounded-md text-sm font-medium transition-all ${
                orderType === 'sell' ? 'bg-red-600 text-white' : 'text-gray-500 hover:text-gray-300'
              }`}
            >
              Sell VTR
            </button>
          </div>

          <div className="space-y-4">
            <div>
              <label className="label">Amount (VTR)</label>
              <input
                type="number"
                className="input-field font-mono"
                placeholder="0.00000000"
                value={amount}
                onChange={e => setAmount(e.target.value)}
              />
            </div>
            <div>
              <label className="label">Price per VTR (BTC)</label>
              <input
                type="number"
                className="input-field font-mono"
                placeholder="0.00000000"
                value={price}
                onChange={e => setPrice(e.target.value)}
              />
            </div>
            <div className="bg-navy-800/60 rounded-lg p-3 flex justify-between text-sm">
              <span className="text-gray-500">Total</span>
              <span className="font-mono text-gray-300">{total} BTC</span>
            </div>

            <div className="flex gap-2 bg-navy-800/40 border border-navy-700/40 rounded-lg p-3 text-xs text-gray-500">
              <Info size={13} className="flex-shrink-0 mt-0.5 text-gray-600" />
              <p>Orders are broadcast to the vTorrent P2P network. When matched, an HTLC contract is created automatically. The swap completes atomically — both sides succeed or neither does.</p>
            </div>

            <button
              disabled={!amount || !price}
              className={`w-full font-semibold py-2.5 rounded-lg transition-all ${
                orderType === 'buy'
                  ? 'bg-emerald-600 hover:bg-emerald-500 text-white disabled:opacity-50'
                  : 'bg-red-600 hover:bg-red-500 text-white disabled:opacity-50'
              }`}
            >
              {orderType === 'buy' ? 'Place Buy Order' : 'Place Sell Order'}
            </button>
          </div>
        </div>
      )}

      {/* My Orders — live data filtered by status */}
      {tab === 'myorders' && (
        <div className="card">
          <h3 className="font-semibold text-white text-sm mb-4">My Orders</h3>
          {error ? (
            <div className="flex items-center gap-2 text-xs text-red-400 py-4">
              <AlertCircle size={13} />
              Could not load orders — node may be offline.
            </div>
          ) : orders.length === 0 ? (
            <p className="text-xs text-gray-600 py-4">No orders yet. Create one in the "Create Order" tab.</p>
          ) : (
            <div className="space-y-3">
              {orders.map((order: DexOrder) => {
                const side = orderSide(order)
                return (
                  <div key={order.id} className="flex items-center gap-4 py-3 border-b border-navy-800/60 last:border-0">
                    <div className={`w-2 h-2 rounded-full flex-shrink-0 ${
                      side === 'buy' ? 'bg-emerald-400' : 'bg-red-400'
                    }`} />
                    <div className="flex-1">
                      <div className="flex items-center gap-2">
                        <span className={`text-xs font-medium ${side === 'buy' ? 'text-emerald-400' : 'text-red-400'}`}>
                          {side.toUpperCase()}
                        </span>
                        <span className="text-xs text-gray-400">
                          {formatVTR(side === 'sell' ? order.offerAmountSatoshis : order.requestAmountSatoshis)}
                        </span>
                        <span className="text-xs text-gray-600">@ {order.rate.toFixed(8)}</span>
                      </div>
                      <p className="text-xs text-gray-600 mt-0.5">{createdAgo(order.createdAt)}</p>
                    </div>
                    <div className="flex items-center gap-2">
                      {order.status === 'open' && (
                        <>
                          <span className="badge-yellow">Open</span>
                          <button
                            onClick={() => handleCancel(order.id)}
                            disabled={cancelling === order.id}
                            className="text-xs text-red-400 hover:text-red-300 transition-colors disabled:opacity-50"
                          >
                            {cancelling === order.id ? 'Cancelling…' : 'Cancel'}
                          </button>
                        </>
                      )}
                      {order.status === 'completed' && (
                        <span className="badge-green flex items-center gap-1"><CheckCircle size={10} /> Completed</span>
                      )}
                      {order.status === 'cancelled' && (
                        <span className="badge-red flex items-center gap-1"><XCircle size={10} /> Cancelled</span>
                      )}
                      {order.status === 'matched' && (
                        <span className="badge-blue">Matched</span>
                      )}
                    </div>
                  </div>
                )
              })}
            </div>
          )}
        </div>
      )}

      {/* Swap lifecycle */}
      {tab === 'swap' && (
        <div className="max-w-md card">
          <h3 className="font-semibold text-white text-sm mb-4 flex items-center gap-2">
            <ArrowRightLeft size={16} className="text-vtorrent-400" />
            Swap Lifecycle
          </h3>
          <p className="text-xs text-gray-500 mb-4">
            Drive an atomic swap through its stages: match (fund VTR), fund BTC, claim VTR, claim BTC, or refund after expiry.
          </p>

          <div className="space-y-4">
            <div>
              <label className="label">Order ID</label>
              <input
                className="input-field font-mono"
                placeholder="Hex order ID"
                value={swapOrderId}
                onChange={e => setSwapOrderId(e.target.value)}
              />
            </div>

            <div>
              <label className="label">Taker VTR Address</label>
              <input
                className="input-field font-mono"
                placeholder="V…"
                value={takerAddress}
                onChange={e => setTakerAddress(e.target.value)}
              />
            </div>

            <div>
              <label className="label">BTC Refund Address</label>
              <input
                className="input-field font-mono"
                placeholder="bc1q…"
                value={btcRefundAddress}
                onChange={e => setBtcRefundAddress(e.target.value)}
              />
            </div>

            <div>
              <label className="label">Preimage (hex, for claim)</label>
              <input
                className="input-field font-mono"
                placeholder="64 hex chars"
                value={preimage}
                onChange={e => setPreimage(e.target.value)}
              />
            </div>

            {swapError && (
              <div className="flex gap-2 bg-red-900/20 border border-red-800/40 rounded-lg p-3">
                <AlertCircle size={16} className="text-red-400 flex-shrink-0 mt-0.5" />
                <p className="text-red-300 text-sm">{swapError}</p>
              </div>
            )}
            {swapResult && (
              <div className="flex gap-2 bg-emerald-900/20 border border-emerald-800/40 rounded-lg p-3">
                <CheckCircle size={16} className="text-emerald-400 flex-shrink-0 mt-0.5" />
                <p className="text-emerald-300 text-sm">{swapResult}</p>
              </div>
            )}

            <div className="grid grid-cols-2 gap-2">
              <button
                disabled={swapBusy || !swapOrderId || !takerAddress}
                onClick={() => runSwap(
                  () => matchDexOrder({ orderId: swapOrderId, takerAddress, passphrase: '' }),
                  'Order matched — VTR HTLC funded'
                )}
                className="btn-primary text-xs disabled:opacity-50"
              >
                Match (Fund VTR)
              </button>
              <button
                disabled={swapBusy || !swapOrderId || !btcRefundAddress}
                onClick={() => runSwap(
                  () => btcFund({ orderId: swapOrderId, btcRefundAddress }),
                  'BTC HTLC funded'
                )}
                className="btn-primary text-xs disabled:opacity-50"
              >
                Fund BTC
              </button>
              <button
                disabled={swapBusy || !swapOrderId || !preimage}
                onClick={() => runSwap(
                  () => vtrClaim({ orderId: swapOrderId, preimage }),
                  'VTR claimed'
                )}
                className="btn-primary text-xs disabled:opacity-50"
              >
                Claim VTR
              </button>
              <button
                disabled={swapBusy || !swapOrderId}
                onClick={() => runSwap(
                  () => btcClaim(swapOrderId),
                  'BTC claimed'
                )}
                className="btn-primary text-xs disabled:opacity-50"
              >
                Claim BTC
              </button>
              <button
                disabled={swapBusy || !swapOrderId}
                onClick={() => runSwap(
                  () => swapRefund(swapOrderId),
                  'Swap refunded'
                )}
                className="btn-secondary text-xs disabled:opacity-50 col-span-2"
              >
                Refund
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
