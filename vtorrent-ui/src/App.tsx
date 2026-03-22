import { Routes, Route, Navigate } from 'react-router-dom'
import { WalletProvider, useWallet } from './hooks/useWallet'
import WelcomePage from './pages/WelcomePage'
import ImportWizardPage from './pages/ImportWizardPage'
import CreateWalletPage from './pages/CreateWalletPage'
import DashboardPage from './pages/DashboardPage'
import SecurityCenterPage from './pages/SecurityCenterPage'
import TorrentPage from './pages/TorrentPage'
import TradePage from './pages/TradePage'
import StakingPage from './pages/StakingPage'
import LegacyClaimPage from './pages/LegacyClaimPage'
import Layout from './components/Layout'

function AppRoutes() {
  const { isUnlocked } = useWallet()

  return (
    <Routes>
      {/* Public routes (no wallet needed) */}
      <Route path="/" element={<WelcomePage />} />
      <Route path="/create" element={<CreateWalletPage />} />
      <Route path="/import" element={<ImportWizardPage />} />

      {/* Protected routes (wallet must be unlocked) */}
      <Route element={<Layout />}>
        <Route
          path="/dashboard"
          element={isUnlocked ? <DashboardPage /> : <Navigate to="/" replace />}
        />
        <Route
          path="/security"
          element={isUnlocked ? <SecurityCenterPage /> : <Navigate to="/" replace />}
        />
        <Route
          path="/torrents"
          element={isUnlocked ? <TorrentPage /> : <Navigate to="/" replace />}
        />
        <Route
          path="/trade"
          element={isUnlocked ? <TradePage /> : <Navigate to="/" replace />}
        />
        <Route
          path="/staking"
          element={isUnlocked ? <StakingPage /> : <Navigate to="/" replace />}
        />
        <Route
          path="/claim"
          element={isUnlocked ? <LegacyClaimPage /> : <Navigate to="/" replace />}
        />
      </Route>

      {/* Fallback */}
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  )
}

export default function App() {
  return (
    <WalletProvider>
      <AppRoutes />
    </WalletProvider>
  )
}
