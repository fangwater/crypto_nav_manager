import { lazy, Suspense } from 'react'
import { Route, Routes } from 'react-router-dom'
import './App.css'
import { IndexPage } from './pages/IndexPage'

const StrategyPage = lazy(() =>
  import('./pages/PnlStrategyPage').then((module) => ({
    default: module.PnlStrategyPage,
  })),
)

export default function App() {
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
        <Route path="/strategies/:slug" element={<StrategyPage />} />
      </Routes>
    </Suspense>
  )
}

