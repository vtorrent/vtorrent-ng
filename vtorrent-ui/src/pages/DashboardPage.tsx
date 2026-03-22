import { useState } from 'react'
import { Copy, Plus, RefreshCw, TrendingUp, Coins, ArrowUpRight, ArrowDownLeft, Clock } from 'lucide-react'
import { useWallet, formatVTR } from '../hooks/useWallet'

export default function DashboardPage() {
  const { keys, defaultAddress, totalBalance, legacyImportCount, generateAddress } = useWallet()
  const [copied, setCopied] = useState('')
  const [generatingKey, setGeneratingKey] = useState(false)

  const copyAddress = (addr: string) => {
    navigator.clipboard.writeText(addr)
    setCopied(addr)
    setTimeout(() => setCopied(''), 2000)
  }

  const handleGenerateKey = async () => {
    setGeneratingKey(true)
    try {
      await generateAddress()
    } finally {
      setGeneratingKey(false)
    }
  }

  // Mock recent transactions for UI preview
  const mockTxs = [
    { type: 'receive', amount: 1500000000, address: 'V3kRm...9xPq', time: '2h ago', label: 'Seeding reward' },
    { type: 'send',    amount: 500000000,  address: 'V7nBw...4mLs', time: '1d ago', label: 'P2P trade' },
    { type: 'stake',   amount: 125000000,  address: 'Stake reward',  time: '2d ago', label: 'PoS reward' },
  ]

  return (
    <div className="p-6 space-y-6">
      {/* Page header */}
      <div>
        <h1 className="text-xl font-bold text-white">Dashboard</h1>
        <p className="text-gray-500 text-sm mt-0.5">Your vTorrent wallet overview</p>
      </div>

      {/* Stats row */}
      <div className="grid grid-cols-3 gap-4">
        <div className="card">
          <p className="text-xs text-gray-500 mb-1">Total Balance</p>
          <p className="text-xl font-bold text-vtorrent-300 font-mono">{formatVTR(totalBalance)}</p>
          <p className="text-xs text-gray-600 mt-1">≈ $0.00 USD</p>
        </div>
        <div className="card">
          <p className="text-xs text-gray-500 mb-1">Addresses</p>
          <p className="text-xl font-bold text-white">{keys.length}</p>
          {legacyImportCount > 0 && (
            <p className="text-xs text-amber-400 mt-1">{legacyImportCount} legacy imported</p>
          )}
        </div>
        <div className="card">
          <p className="text-xs text-gray-500 mb-1">Staking Status</p>
          <p className="text-xl font-bold text-emerald-400">Active</p>
          <p className="text-xs text-gray-600 mt-1">~5% APR</p>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-4">
        {/* Addresses */}
        <div className="card">
          <div className="flex items-center justify-between mb-4">
            <h2 className="font-semibold text-white text-sm">Addresses</h2>
            <button
              onClick={handleGenerateKey}
              disabled={generatingKey}
              className="flex items-center gap-1.5 text-xs text-vtorrent-400 hover:text-vtorrent-300 transition-colors"
            >
              {generatingKey
                ? <RefreshCw size={12} className="animate-spin" />
                : <Plus size={12} />
              }
              New Address
            </button>
          </div>

          <div className="space-y-2">
            {keys.map(key => (
              <div
                key={key.address}
                className={`rounded-lg p-3 border transition-all ${
                  key.address === defaultAddress
                    ? 'bg-vtorrent-500/8 border-vtorrent-700/40'
                    : 'bg-navy-800/40 border-navy-700/40'
                }`}
              >
                <div className="flex items-start justify-between gap-2">
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2 mb-1">
                      {key.address === defaultAddress && (
                        <span className="text-xs text-vtorrent-400 font-medium">Default</span>
                      )}
                      {key.isLegacyImport && (
                        <span className="badge-yellow text-xs">Legacy</span>
                      )}
                    </div>
                    <p className="font-mono text-xs text-vtorrent-300 truncate">{key.address}</p>
                    {key.label && (
                      <p className="text-xs text-gray-500 mt-0.5">{key.label}</p>
                    )}
                  </div>
                  <button
                    onClick={() => copyAddress(key.address)}
                    className="text-gray-600 hover:text-vtorrent-400 transition-colors flex-shrink-0"
                  >
                    {copied === key.address
                      ? <span className="text-xs text-vtorrent-400">Copied!</span>
                      : <Copy size={12} />
                    }
                  </button>
                </div>
                <div className="mt-2 flex items-center justify-between">
                  <span className="font-mono text-xs text-gray-400">{formatVTR(key.balance)}</span>
                </div>
              </div>
            ))}
          </div>
        </div>

        {/* Recent transactions */}
        <div className="card">
          <h2 className="font-semibold text-white text-sm mb-4">Recent Transactions</h2>
          <div className="space-y-3">
            {mockTxs.map((tx, i) => (
              <div key={i} className="flex items-center gap-3 py-2 border-b border-navy-800/60 last:border-0">
                <div className={`w-8 h-8 rounded-lg flex items-center justify-center flex-shrink-0 ${
                  tx.type === 'receive' ? 'bg-emerald-900/30' :
                  tx.type === 'send' ? 'bg-red-900/30' : 'bg-vtorrent-900/30'
                }`}>
                  {tx.type === 'receive' ? <ArrowDownLeft size={14} className="text-emerald-400" /> :
                   tx.type === 'send' ? <ArrowUpRight size={14} className="text-red-400" /> :
                   <TrendingUp size={14} className="text-vtorrent-400" />}
                </div>
                <div className="flex-1 min-w-0">
                  <p className="text-xs font-medium text-gray-300">{tx.label}</p>
                  <p className="text-xs text-gray-600 truncate">{tx.address}</p>
                </div>
                <div className="text-right flex-shrink-0">
                  <p className={`text-xs font-mono font-medium ${
                    tx.type === 'receive' || tx.type === 'stake' ? 'text-emerald-400' : 'text-red-400'
                  }`}>
                    {tx.type === 'send' ? '-' : '+'}{formatVTR(tx.amount)}
                  </p>
                  <p className="text-xs text-gray-600">{tx.time}</p>
                </div>
              </div>
            ))}
          </div>

          {/* Claim legacy VTR CTA */}
          {legacyImportCount > 0 && (
            <div className="mt-4 bg-amber-900/15 border border-amber-800/30 rounded-lg p-3">
              <div className="flex items-center gap-2 mb-1">
                <Coins size={14} className="text-amber-400" />
                <p className="text-amber-300 text-xs font-medium">Legacy VTR Ready to Claim</p>
              </div>
              <p className="text-gray-500 text-xs">
                You have {legacyImportCount} imported legacy address{legacyImportCount !== 1 ? 'es' : ''}.
                Claim your VTR once the new chain launches.
              </p>
              <button className="mt-2 text-xs text-amber-400 hover:text-amber-300 font-medium transition-colors">
                Claim Now →
              </button>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
