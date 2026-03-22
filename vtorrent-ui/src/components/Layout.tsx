import { Outlet, NavLink, useNavigate } from 'react-router-dom'
import {
  LayoutDashboard, Shield, Download, ArrowLeftRight,
  Lock, Wifi, WifiOff, RefreshCw, Cpu, Zap, Gift,
} from 'lucide-react'
import { useWallet, formatVTR } from '../hooks/useWallet'
import { useNodeInfo } from '../hooks/useNode'
import clsx from 'clsx'

const navItems = [
  { to: '/dashboard', icon: LayoutDashboard, label: 'Dashboard' },
  { to: '/torrents',  icon: Download,        label: 'Torrents'  },
  { to: '/trade',     icon: ArrowLeftRight,  label: 'P2P Trade' },
  { to: '/staking',   icon: Zap,             label: 'Staking'   },
  { to: '/claim',     icon: Gift,            label: 'Claim VTR' },
  { to: '/security',  icon: Shield,          label: 'Security'  },
]

export default function Layout() {
  const { lock, totalBalance, has2FA, keys } = useWallet()
  const navigate = useNavigate()

  // Poll node info every 8 seconds for live sidebar status
  const { data: nodeInfo, loading: nodeLoading } = useNodeInfo(8_000)

  const handleLock = () => {
    lock()
    navigate('/')
  }

  // Derive status display values
  const isOnline = !!nodeInfo
  const isSyncing = nodeInfo ? nodeInfo.syncing : false
  const syncPct = nodeInfo?.syncPercent ?? 0
  const blockHeight = nodeInfo?.blockHeight ?? 0
  const peerCount = nodeInfo?.connections ?? 0
  const mempoolCount = nodeInfo?.mempoolSize ?? 0

  return (
    <div className="flex h-screen overflow-hidden gradient-bg">
      {/* Sidebar */}
      <aside className="w-56 flex-shrink-0 bg-navy-900/80 border-r border-vtorrent-900/30 flex flex-col">
        {/* Logo */}
        <div className="px-5 py-5 border-b border-vtorrent-900/30">
          <div className="flex items-center gap-2.5">
            <div className="w-8 h-8 rounded-lg bg-vtorrent-500/20 border border-vtorrent-500/40 flex items-center justify-center">
              <span className="text-vtorrent-400 font-bold text-sm">VT</span>
            </div>
            <div>
              <p className="text-white font-semibold text-sm leading-none">vTorrent</p>
              <p className="text-vtorrent-500 text-xs mt-0.5">v2.0.0</p>
            </div>
          </div>
        </div>

        {/* Balance summary */}
        <div className="px-4 py-4 border-b border-vtorrent-900/30">
          <p className="text-xs text-gray-500 mb-1">Total Balance</p>
          <p className="text-vtorrent-300 font-mono font-medium text-sm">{formatVTR(totalBalance)}</p>
          <p className="text-xs text-gray-500 mt-1">{keys.length} address{keys.length !== 1 ? 'es' : ''}</p>
        </div>

        {/* Navigation */}
        <nav className="flex-1 px-3 py-3 space-y-1 overflow-y-auto">
          {navItems.map(({ to, icon: Icon, label }) => (
            <NavLink
              key={to}
              to={to}
              className={({ isActive }) => clsx(
                'flex items-center gap-3 px-3 py-2.5 rounded-lg text-sm font-medium transition-all duration-150',
                isActive
                  ? 'bg-vtorrent-500/15 text-vtorrent-300 border border-vtorrent-500/20'
                  : 'text-gray-400 hover:text-gray-200 hover:bg-navy-800/60'
              )}
            >
              <Icon size={16} />
              {label}
            </NavLink>
          ))}
        </nav>

        {/* Node Status Panel */}
        <div className="px-4 py-4 border-t border-vtorrent-900/30 space-y-3">

          {/* Online / Offline indicator */}
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              {nodeLoading && !nodeInfo ? (
                <RefreshCw size={11} className="text-gray-600 animate-spin" />
              ) : isOnline ? (
                <Wifi size={11} className="text-emerald-400" />
              ) : (
                <WifiOff size={11} className="text-gray-600" />
              )}
              <span className={clsx(
                'text-xs font-medium',
                isOnline ? 'text-emerald-400' : 'text-gray-600'
              )}>
                {nodeLoading && !nodeInfo ? 'Connecting…' : isOnline ? 'Online' : 'Offline'}
              </span>
            </div>
            {isOnline && (
              <span className="text-xs text-gray-600">
                {peerCount} peer{peerCount !== 1 ? 's' : ''}
              </span>
            )}
          </div>

          {/* Block height */}
          {isOnline && (
            <div className="space-y-1.5">
              <div className="flex items-center justify-between">
                <span className="text-xs text-gray-600">Block</span>
                <span className="text-xs text-gray-400 font-mono">
                  {blockHeight.toLocaleString()}
                </span>
              </div>

              {/* Sync progress bar */}
              {isSyncing ? (
                <div>
                  <div className="flex items-center justify-between mb-1">
                    <span className="text-xs text-amber-400 flex items-center gap-1">
                      <RefreshCw size={9} className="animate-spin" />
                      Syncing
                    </span>
                    <span className="text-xs text-gray-600">{syncPct.toFixed(1)}%</span>
                  </div>
                  <div className="w-full h-1 bg-navy-800 rounded-full overflow-hidden">
                    <div
                      className="h-full bg-vtorrent-500 rounded-full transition-all duration-500"
                      style={{ width: `${Math.min(syncPct, 100)}%` }}
                    />
                  </div>
                </div>
              ) : (
                <div className="flex items-center gap-1.5">
                  <div className="w-1.5 h-1.5 rounded-full bg-emerald-400 animate-pulse-slow" />
                  <span className="text-xs text-emerald-400">Fully synced</span>
                </div>
              )}

              {/* Mempool */}
              {mempoolCount > 0 && (
                <div className="flex items-center justify-between">
                  <span className="text-xs text-gray-600 flex items-center gap-1">
                    <Cpu size={9} />
                    Mempool
                  </span>
                  <span className="text-xs text-gray-500">
                    {mempoolCount.toLocaleString()} tx{mempoolCount !== 1 ? 's' : ''}
                  </span>
                </div>
              )}
            </div>
          )}

          {/* 2FA status */}
          {has2FA && (
            <div className="badge-green text-xs">
              <Shield size={10} />
              2FA Active
            </div>
          )}

          {/* Lock button */}
          <button
            onClick={handleLock}
            className="w-full flex items-center gap-2 px-3 py-2 rounded-lg text-sm text-gray-400 hover:text-red-400 hover:bg-red-900/10 transition-all duration-150"
          >
            <Lock size={14} />
            Lock Wallet
          </button>
        </div>
      </aside>

      {/* Main content */}
      <main className="flex-1 overflow-y-auto">
        <Outlet />
      </main>
    </div>
  )
}
