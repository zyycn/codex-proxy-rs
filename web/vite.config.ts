import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vite'

function shouldIgnoreRolldownLog(log: { code?: string; id?: string; loc?: { file?: string } }) {
  const source = String(log.id ?? log.loc?.file ?? '')
  return log.code === 'INVALID_ANNOTATION' && source.includes('@vueuse/core')
}

export default defineConfig({
  base: '/',
  plugins: [vue(), tailwindcss()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  server: {
    port: 5173,
    proxy: {
      '/dev': {
        target: 'http://127.0.0.1:8080',
        changeOrigin: true,
        rewrite: (path) => path.replace(/^\/dev/, ''),
      },
    },
  },
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    rolldownOptions: {
      onLog(level, log, handler) {
        if (level === 'warn' && shouldIgnoreRolldownLog(log)) {
          return
        }

        handler(level, log)
      },
      output: {
        codeSplitting: {
          groups: [
            {
              name: 'echarts',
              test: /node_modules\/echarts|node_modules\/zrender/,
            },
          ],
        },
      },
    },
  },
})
