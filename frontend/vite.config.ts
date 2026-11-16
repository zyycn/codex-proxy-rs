import { fileURLToPath, URL } from 'node:url'
import CodexProxyUI from '@codex-proxy/ui/vite'
import VueI18n from '@intlify/unplugin-vue-i18n/vite'
import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { defineConfig, loadEnv } from 'vite'
import { pluginBridge } from './build/plugin-bridge.ts'

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, fileURLToPath(new URL('.', import.meta.url)), 'CPR_WEB_BASE_PATH')
  const sourceUi = mode === 'source'
  const uiRoot = new URL('../modules/ui/', import.meta.url)

  return {
    base: env.CPR_WEB_BASE_PATH || '/',
    plugins: [
      vue(),
      VueI18n({
        include: fileURLToPath(new URL('./src/i18n/locales/**/*.json', import.meta.url)),
      }),
      tailwindcss(),
      CodexProxyUI({
        source: sourceUi ? uiRoot : undefined,
      }),
      pluginBridge(),
    ],
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
          cookiePathRewrite: { '/api/auth': '/dev/api/auth' },
          rewrite: path => path.replace(/^\/dev/, ''),
        },
      },
    },
    build: {
      outDir: sourceUi ? 'node_modules/.vite/source-dist' : 'dist',
      assetsDir: 'assets',
      license: { fileName: 'THIRD_PARTY_LICENSES.md' },
      chunkSizeWarningLimit: 600,
      rolldownOptions: {
        checks: {
          invalidAnnotation: false,
        },
        output: {
          codeSplitting: {
            groups: [
              {
                name: 'echarts',
                test: /node_modules\/echarts/,
              },
              {
                name: 'zrender',
                test: /node_modules\/zrender/,
              },
            ],
          },
        },
      },
    },
  }
})
