import { Outlet, NavLink, useNavigate } from 'react-router-dom'
import {
  LayoutDashboard, Shield, Download, ArrowLeftRight,
  Lock, Wifi, WifiOff, RefreshCw, Cpu, Zap, Gift, Bitcoin, Palette, Coins, Bell, Landmark, History, Settings,
} from 'lucide-react'
import { useWallet, formatVTR } from '../hooks/useWallet'
import { useNodeInfo } from '../hooks/useNode'
import { useNotifications } from '../hooks/useNotifications'
import { useTheme } from '../hooks/useTheme'
import AppIcon from './AppIcon'
import clsx from 'clsx'

const navGroups = [
  {
    label: 'Wallet',
    items: [
      { to: '/dashboard', icon: LayoutDashboard, label: 'Dashboard' },
      { to: '/activity',  icon: History,         label: 'Activity'  },
      { to: '/btc',       icon: Bitcoin,         label: 'Bitcoin'   },
      { to: '/claim',     icon: Gift,            label: 'Claim VTR' },
    ],
  },
  {
    label: 'Earn',
    items: [
      { to: '/staking',   icon: Zap,             label: 'Staking'   },
      { to: '/torrents',  icon: Download,        label: 'Torrents'  },
      { to: '/earnings',  icon: Coins,           label: 'Earnings'  },
    ],
  },
  {
    label: 'Trade',
    items: [
      { to: '/trade',     icon: ArrowLeftRight,  label: 'P2P Trade' },
      { to: '/governance', icon: Landmark,       label: 'Governance' },
    ],
  },
  {
    label: 'System',
    items: [
      { to: '/security',  icon: Shield,          label: 'Security'  },
      { to: '/settings',  icon: Settings,        label: 'Settings'  },
    ],
  },
]

export default function Layout() {
  const { lock, totalBalance, has2FA, keys } = useWallet()
  const { theme, toggle: toggleTheme } = useTheme()
  const { unread } = useNotifications()
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
      <aside className="w-16 lg:w-56 flex-shrink-0 bg-navy-900/80 border-r border-vtorrent-900/30 flex flex-col">
        {/* Logo */}
        <div className="px-2 lg:px-5 py-5 border-b border-vtorrent-900/30">
          <div className="flex items-center justify-center lg:justify-start gap-2.5">
            <AppIcon size={32} />
            <div className="hidden lg:block">
              <p className="text-white font-semibold text-sm leading-none">vTorrent</p>
              <p className="text-vtorrent-500 text-xs mt-0.5">v2.0.0-beta.3</p>
            </div>
          </div>
        </div>

        {/* Balance summary */}
        <div className="hidden lg:block px-4 py-4 border-b border-vtorrent-900/30">
          <p className="text-xs text-gray-500 mb-1">Total Balance</p>
          <p className="text-vtorrent-300 font-mono font-medium text-sm">{formatVTR(totalBalance)}</p>
          <p className="text-xs text-gray-500 mt-1">{keys.length} address{keys.length !== 1 ? 'es' : ''}</p>
        </div>

        {/* Navigation */}
        <nav aria-label="Primary" className="flex-1 px-2 lg:px-3 py-3 space-y-4 overflow-y-auto">
          {navGroups.map(group => (
            <div key={group.label}>
              <p className="hidden lg:block px-3 pb-1 text-[10px] font-semibold uppercase tracking-wider text-gray-600">
                {group.label}
              </p>
              <div className="space-y-1">
                {group.items.map(({ to, icon: Icon, label }) => (
                  <NavLink
                    key={to}
                    to={to}
                    title={label}
                    aria-label={label}
                    className={({ isActive }) => clsx(
                      'flex items-center justify-center lg:justify-start gap-3 px-3 py-2.5 rounded-lg text-sm font-medium transition-all duration-150',
                      'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-vtorrent-400 focus-visible:ring-offset-2 focus-visible:ring-offset-navy-900',
                      isActive
                        ? 'bg-vtorrent-500/15 text-vtorrent-300 border border-vtorrent-500/20'
                        : 'text-gray-400 hover:text-gray-200 hover:bg-navy-800/60'
                    )}
                  >
                    <Icon size={16} />
                    <span className="hidden lg:inline">{label}</span>
                  </NavLink>
                ))}
              </div>
            </div>
          ))}
        </nav>

        {/* Node Status Panel */}
        <div className="px-2 lg:px-4 py-4 border-t border-vtorrent-900/30 space-y-3">

          {/* Online / Offline indicator */}
          <div className="hidden lg:flex items-center justify-between">
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
          {nodeInfo && nodeInfo.network !== 'vtorrent-mainnet' && (
            <span className="hidden lg:inline-flex w-fit items-center text-[10px] font-bold tracking-widest text-amber-400 border border-amber-500/40 bg-amber-500/10 rounded px-1.5 py-0.5">
              TESTNET
            </span>
          )}

          {/* Block height */}
          {isOnline && (
            <div className="hidden lg:block space-y-1.5">
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
            <div className="hidden lg:flex badge-green text-xs">
              <Shield size={10} />
              2FA Active
            </div>
          )}

          <div className="lg:hidden flex justify-center">
            <span
              className={clsx(
                'w-2 h-2 rounded-full',
                isOnline ? 'bg-emerald-400' : 'bg-gray-600'
              )}
              title={isOnline ? 'Node online' : 'Node offline'}
              aria-label={isOnline ? 'Node online' : 'Node offline'}
            />
          </div>

          {/* Notifications */}
          <button
            onClick={() => navigate('/notifications')}
            aria-label="Notifications"
            title="Notifications"
            className="w-full flex items-center justify-center lg:justify-start gap-2 px-3 py-2 rounded-lg text-sm text-gray-400 hover:text-gray-200 hover:bg-navy-800/60 transition-all duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-vtorrent-400 focus-visible:ring-offset-2 focus-visible:ring-offset-navy-900"
          >
            <Bell size={14} />
            <span className="hidden lg:inline">Notifications</span>
            {unread > 0 && (
              <span className="ml-auto badge-green text-xs px-1.5 py-0.5">{unread}</span>
            )}
          </button>

          {/* Theme toggle */}
          <button
            onClick={toggleTheme}
            className="w-full flex items-center justify-center lg:justify-start gap-2 px-3 py-2 rounded-lg text-sm text-gray-400 hover:text-gray-200 hover:bg-navy-800/60 transition-all duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-vtorrent-400 focus-visible:ring-offset-2 focus-visible:ring-offset-navy-900"
            title={theme === 'legacy' ? 'Switch to modern theme' : 'Switch to legacy theme'}
            aria-label={theme === 'legacy' ? 'Switch to modern theme' : 'Switch to legacy theme'}
          >
            <Palette size={14} />
            <span className="hidden lg:inline">{theme === 'legacy' ? 'Modern Theme' : 'Legacy Theme'}</span>
          </button>

          {/* Lock button */}
          <button
            onClick={handleLock}
            aria-label="Lock Wallet"
            title="Lock Wallet"
            className="w-full flex items-center justify-center lg:justify-start gap-2 px-3 py-2 rounded-lg text-sm text-gray-400 hover:text-red-400 hover:bg-red-900/10 transition-all duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-vtorrent-400 focus-visible:ring-offset-2 focus-visible:ring-offset-navy-900"
          >
            <Lock size={14} />
            <span className="hidden lg:inline">Lock Wallet</span>
          </button>
        </div>
      </aside>

      {/* Main content */}
      <main className="flex-1 min-w-0 overflow-y-auto">
        <Outlet />
      </main>
    </div>
  )
}
