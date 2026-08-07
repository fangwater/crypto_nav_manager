import { lazy, Suspense, useEffect, useState } from 'react'
import { Route, Routes } from 'react-router-dom'
import './App.css'
import { getHealth } from './api'
import { IndexPage } from './pages/IndexPage'

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

export default function App() {
  const [readOnly, setReadOnly] = useState(true)

  useEffect(() => {
    const controller = new AbortController()
    getHealth(controller.signal)
      .then((health) => setReadOnly(health.readOnly))
      .catch(() => setReadOnly(true))
    return () => controller.abort()
  }, [])

  return (
    <Suspense
      fallback={
        <main className="detail-shell">
          <div className="detail-loading" />
        </main>
      }
    >
      <Routes>
        <Route path="/" element={<IndexPage />} />
        <Route path="/monitor" element={<OpsMonitorPage />} />
        <Route path="/market-data" element={<MarketDataNetworkPage />} />
        <Route path="/fee-rates" element={<FeeRatesPage readOnly={readOnly} />} />
        <Route path="/intra-matching" element={<IntraMatchingPage />} />
        <Route path="/fr-position-limits" element={<FrPositionLimitsPage />} />
        <Route
          path="/strategies/:slug"
          element={<StrategyPage readOnly={readOnly} />}
        />
      </Routes>
    </Suspense>
  )
}
