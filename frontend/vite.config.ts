import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

const apiTarget = process.env.NAV_API_TARGET ?? 'http://127.0.0.1:4200'

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
    },
  },
})

