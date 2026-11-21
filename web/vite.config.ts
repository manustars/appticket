import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// Where the core listens; the dev server forwards /api there.
const core = process.env.HD_CORE_URL || 'http://127.0.0.1:8080'

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      // The core's REST API.
      '/api': core,
    },
  },
})
