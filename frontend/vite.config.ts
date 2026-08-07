import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

const apiTarget = process.env.NAV_API_TARGET ?? 'http://127.0.0.1:4200'
const opsApiTarget = process.env.OPS_API_TARGET ?? 'http://127.0.0.1:4210'
const marketDataApiTarget =
  process.env.MARKET_DATA_API_TARGET ?? 'http://127.0.0.1:9918'

export default defineConfig({
  base: '/nav/',
  plugins: [react()],
  server: {
    host: '127.0.0.1',
    port: 5173,
    proxy: {
      '/nav-api': {
        target: apiTarget,
        changeOrigin: true,
        rewrite: (path) => path.replace(/^\/nav-api/, '/api'),
      },
      '/ops-api': {
        target: opsApiTarget,
        changeOrigin: true,
        rewrite: (path) => path.replace(/^\/ops-api/, ''),
      },
      '/market-data-api': {
        target: marketDataApiTarget,
        changeOrigin: true,
        rewrite: (path) => path.replace(/^\/market-data-api/, ''),
      },
    },
  },
})

