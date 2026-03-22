import { useState } from 'react'
import {
  ArrowLeftRight, Plus, Clock, CheckCircle, XCircle,
  AlertTriangle, Zap, Shield, Info
} from 'lucide-react'
import { formatVTR } from '../hooks/useWallet'

interface Order {
  id: string
  type: 'buy' | 'sell'
  pair: string
  amount: number
  price: number
  total: number
  maker: string
  status: 'open' | 'matched' | 'completed' | 'cancelled'
  createdAt: string
  expiresIn: string
}

const mockOrderBook: Order[] = [
  { id: '1', type: 'sell', pair: 'VTR/BTC', amount: 10000_00000000, price: 0.00000042, total: 0.0042,  maker: 'V3kRm...9xPq', status: 'open', createdAt: '5m ago', expiresIn: '23h' },
  { id: '2', type: 'sell', pair: 'VTR/BTC', amount: 5000_00000000,  price: 0.00000044, total: 0.0022,  maker: 'V7nBw...4mLs', status: 'open', createdAt: '12m ago', expiresIn: '11h' },
  { id: '3', type: 'sell', pair: 'VTR/BTC', amount: 25000_00000000, price: 0.00000045, total: 0.01125, maker: 'V9pXz...2qRt', status: 'open', createdAt: '1h ago', expiresIn: '6h' },
  { id: '4', type: 'buy',  pair: 'VTR/BTC', amount: 8000_00000000,  price: 0.00000040, total: 0.0032,  maker: 'V2mKs...7wNp', status: 'open', createdAt: '3m ago', expiresIn: '47h' },
  { id: '5', type: 'buy',  pair: 'VTR/BTC', amount: 15000_00000000, price: 0.00000038, total: 0.0057,  maker: 'V6hJq...1vCx', status: 'open', createdAt: '30m ago', expiresIn: '20h' },
]

const myOrders: Order[] = [
  { id: '6', type: 'sell', pair: 'VTR/BTC', amount: 2000_00000000, price: 0.00000043, total: 0.00086, maker: 'You', status: 'open', createdAt: '2h ago', expiresIn: '22h' },
  { id: '7', type: 'buy',  pair: 'VTR/BTC', amount: 5000_00000000, price: 0.00000039, total: 0.00195, maker: 'You', status: 'completed', createdAt: '1d ago', expiresIn: '—' },
]

export default function TradePage() {
  const [tab, setTab] = useState<'orderbook' | 'create' | 'myorders'>('orderbook')
  const [orderType, setOrderType] = useState<'buy' | 'sell'>('buy')
  const [amount, setAmount] = useState('')
  const [price, setPrice] = useState('')

  const total = amount && price ? (parseFloat(amount) * parseFloat(price)).toFixed(8) : '0'

  const sells = mockOrderBook.filter(o => o.type === 'sell').sort((a, b) => a.price - b.price)
  const buys  = mockOrderBook.filter(o => o.type === 'buy').sort((a, b) => b.price - a.price)
  const spread = sells.length && buys.length
    ? ((sells[0].price - buys[0].price) / sells[0].price * 100).toFixed(2)
    : null

  return (
    <div className="p-6 space-y-6">
      {/* Header */}
      <div>
        <h1 className="text-xl font-bold text-white">P2P Trade</h1>
        <p className="text-gray-500 text-sm mt-0.5">Trade VTR directly with other users via atomic swaps. No exchange. No custodian.</p>
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
        {(['orderbook', 'create', 'myorders'] as const).map(t => (
          <button
            key={t}
            onClick={() => setTab(t)}
            className={`px-4 py-2 rounded-lg text-sm font-medium transition-all ${
              tab === t
                ? 'bg-vtorrent-500/15 text-vtorrent-300 border border-vtorrent-500/20'
                : 'text-gray-500 hover:text-gray-300'
            }`}
          >
            {t === 'orderbook' ? 'Order Book' : t === 'create' ? 'Create Order' : 'My Orders'}
          </button>
        ))}
      </div>

      {/* Order Book */}
      {tab === 'orderbook' && (
        <div className="grid grid-cols-2 gap-4">
          {/* Sell orders (asks) */}
          <div className="card">
            <h3 className="text-sm font-semibold text-red-400 mb-3">Sell Orders (Asks)</h3>
            <div className="space-y-1">
              <div className="grid grid-cols-3 text-xs text-gray-600 pb-2 border-b border-navy-800">
                <span>Price (BTC)</span>
                <span className="text-right">Amount (VTR)</span>
                <span className="text-right">Expires</span>
              </div>
              {sells.map(order => (
                <div key={order.id} className="grid grid-cols-3 text-xs py-1.5 hover:bg-red-900/10 rounded px-1 cursor-pointer transition-colors group">
                  <span className="text-red-400 font-mono">{order.price.toFixed(8)}</span>
                  <span className="text-right text-gray-300 font-mono">{formatVTR(order.amount)}</span>
                  <span className="text-right text-gray-600">{order.expiresIn}</span>
                </div>
              ))}
            </div>
          </div>

          {/* Buy orders (bids) */}
          <div className="card">
            <h3 className="text-sm font-semibold text-emerald-400 mb-3">Buy Orders (Bids)</h3>
            <div className="space-y-1">
              <div className="grid grid-cols-3 text-xs text-gray-600 pb-2 border-b border-navy-800">
                <span>Price (BTC)</span>
                <span className="text-right">Amount (VTR)</span>
                <span className="text-right">Expires</span>
              </div>
              {buys.map(order => (
                <div key={order.id} className="grid grid-cols-3 text-xs py-1.5 hover:bg-emerald-900/10 rounded px-1 cursor-pointer transition-colors group">
                  <span className="text-emerald-400 font-mono">{order.price.toFixed(8)}</span>
                  <span className="text-right text-gray-300 font-mono">{formatVTR(order.amount)}</span>
                  <span className="text-right text-gray-600">{order.expiresIn}</span>
                </div>
              ))}
            </div>
          </div>

          {spread && (
            <div className="col-span-2 text-center text-xs text-gray-500">
              Spread: <span className="text-vtorrent-400 font-mono">{spread}%</span>
              &nbsp;·&nbsp;
              Best ask: <span className="text-red-400 font-mono">{sells[0]?.price.toFixed(8)}</span>
              &nbsp;·&nbsp;
              Best bid: <span className="text-emerald-400 font-mono">{buys[0]?.price.toFixed(8)}</span>
            </div>
          )}
        </div>
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

      {/* My Orders */}
      {tab === 'myorders' && (
        <div className="card">
          <h3 className="font-semibold text-white text-sm mb-4">My Orders</h3>
          <div className="space-y-3">
            {myOrders.map(order => (
              <div key={order.id} className="flex items-center gap-4 py-3 border-b border-navy-800/60 last:border-0">
                <div className={`w-2 h-2 rounded-full flex-shrink-0 ${
                  order.type === 'buy' ? 'bg-emerald-400' : 'bg-red-400'
                }`} />
                <div className="flex-1">
                  <div className="flex items-center gap-2">
                    <span className={`text-xs font-medium ${order.type === 'buy' ? 'text-emerald-400' : 'text-red-400'}`}>
                      {order.type.toUpperCase()}
                    </span>
                    <span className="text-xs text-gray-400">{formatVTR(order.amount)}</span>
                    <span className="text-xs text-gray-600">@ {order.price.toFixed(8)} BTC</span>
                  </div>
                  <p className="text-xs text-gray-600 mt-0.5">{order.createdAt}</p>
                </div>
                <div className="flex items-center gap-2">
                  {order.status === 'open' && (
                    <>
                      <span className="badge-yellow">Open</span>
                      <button className="text-xs text-red-400 hover:text-red-300 transition-colors">Cancel</button>
                    </>
                  )}
                  {order.status === 'completed' && (
                    <span className="badge-green"><CheckCircle size={10} /> Completed</span>
                  )}
                  {order.status === 'cancelled' && (
                    <span className="badge-red"><XCircle size={10} /> Cancelled</span>
                  )}
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  )
}
