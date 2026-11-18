import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

export default defineConfig({
  plugins: [vue()],
  server: {
    port: 5199,
    proxy: {
      '/api': 'http://127.0.0.1:7450',
      '/metrics': 'http://127.0.0.1:7450',
      '/healthz': 'http://127.0.0.1:7450',
    },
  },
})
