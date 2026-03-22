import { Outlet, NavLink, useNavigate } from 'react-router-dom'
import {
  LayoutDashboard, Shield, Download, ArrowLeftRight,
  Lock, ChevronRight, Wifi, WifiOff
} from 'lucide-react'
import { useWallet, formatVTR } from '../hooks/useWallet'
import clsx from 'clsx'

const navItems = [
  { to: '/dashboard', icon: LayoutDashboard, label: 'Dashboard' },
  { to: '/torrents',  icon: Download,        label: 'Torrents'  },
  { to: '/trade',     icon: ArrowLeftRight,  label: 'P2P Trade' },
  { to: '/security',  icon: Shield,          label: 'Security'  },
]

export default function Layout() {
  const { lock, totalBalance, has2FA, keys } = useWallet()
  const navigate = useNavigate()

  const handleLock = () => {
    lock()
    navigate('/')
  }

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
        <nav className="flex-1 px-3 py-3 space-y-1">
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

        {/* Status & Lock */}
        <div className="px-4 py-4 border-t border-vtorrent-900/30 space-y-3">
          {/* Network status */}
          <div className="flex items-center gap-2">
            <div className="w-1.5 h-1.5 rounded-full bg-emerald-400 animate-pulse-slow" />
            <span className="text-xs text-gray-400">Syncing chain...</span>
          </div>

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
