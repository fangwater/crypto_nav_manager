import { lazy, Suspense, useEffect, useState } from 'react'
import { Navigate, Route, Routes } from 'react-router-dom'
import './App.css'
import {
  getHealth,
  getSession,
  logout,
  setUnauthorizedHandler,
} from './api'
import { IndexPage } from './pages/IndexPage'
import { LoginPage } from './pages/LoginPage'
import type { AuthSession } from './types'

const StrategyPage = lazy(() =>
  import('./pages/PnlStrategyPage').then((module) => ({
    default: module.PnlStrategyPage,
  })),
)

const FeeRatesPage = lazy(() =>
  import('./pages/FeeRatesPage').then((module) => ({
    default: module.FeeRatesPage,
  })),
)

const IntraMatchingPage = lazy(() =>
  import('./pages/IntraMatchingPage').then((module) => ({
    default: module.IntraMatchingPage,
  })),
)

const OpsMonitorPage = lazy(() =>
  import('./pages/OpsMonitorPage').then((module) => ({
    default: module.OpsMonitorPage,
  })),
)

const IntraAnalysisPage = lazy(() =>
  import('./pages/IntraAnalysisPage').then((module) => ({
    default: module.IntraAnalysisPage,
  })),
)

const MarketDataNetworkPage = lazy(() =>
  import('./pages/MarketDataNetworkPage').then((module) => ({
    default: module.MarketDataNetworkPage,
  })),
)

const FrPositionLimitsPage = lazy(() =>
  import('./pages/FrPositionLimitsPage').then((module) => ({
    default: module.FrPositionLimitsPage,
  })),
)

const AdminUsersPage = lazy(() =>
  import('./pages/AdminUsersPage').then((module) => ({
    default: module.AdminUsersPage,
  })),
)

export default function App() {
  const [readOnly, setReadOnly] = useState(true)
  const [session, setSession] = useState<AuthSession | null | undefined>(
    undefined,
  )

  useEffect(() => {
    setUnauthorizedHandler(() => setSession(null))
    const controller = new AbortController()
    getSession(controller.signal)
      .then(setSession)
      .catch(() => setSession(null))
    return () => {
      setUnauthorizedHandler(null)
      controller.abort()
    }
  }, [])

  useEffect(() => {
    if (session == null) return
    const controller = new AbortController()
    getHealth(controller.signal)
      .then((health) => setReadOnly(health.readOnly))
      .catch(() => setReadOnly(true))
    return () => controller.abort()
  }, [session])

  function handleLogout() {
    logout()
      .catch(() => undefined)
      .finally(() => setSession(null))
  }

  if (session === undefined) {
    return (
      <main className="detail-shell">
        <div className="detail-loading" />
      </main>
    )
  }
  if (session === null) {
    return <LoginPage onLogin={setSession} />
  }

  const admin = session.admin
  const writeEnabled = !readOnly && admin
  return (
    <Suspense
      fallback={
        <main className="detail-shell">
          <div className="detail-loading" />
        </main>
      }
    >
      <Routes>
        <Route
          path="/"
          element={<IndexPage session={session} onLogout={handleLogout} />}
        />
        <Route
          path="/monitor"
          element={admin ? <OpsMonitorPage /> : <Navigate to="/" replace />}
        />
        <Route
          path="/market-data"
          element={
            admin ? <MarketDataNetworkPage /> : <Navigate to="/" replace />
          }
        />
        <Route
          path="/admin"
          element={
            admin ? (
              <AdminUsersPage session={session} />
            ) : (
              <Navigate to="/" replace />
            )
          }
        />
        <Route
          path="/fee-rates"
          element={<FeeRatesPage readOnly={!writeEnabled} />}
        />
        <Route path="/intra-matching" element={<IntraMatchingPage />} />
        <Route path="/fr-position-limits" element={<FrPositionLimitsPage />} />
        <Route path="/analysis/:slug" element={<IntraAnalysisPage />} />
        <Route
          path="/strategies/:slug"
          element={<StrategyPage readOnly={!writeEnabled} />}
        />
      </Routes>
    </Suspense>
  )
}
