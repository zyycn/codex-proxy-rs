import { fileURLToPath, URL } from 'node:url'
import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

export default defineConfig(({ mode }) => {
  const sourceUi = mode === 'source'
  const uiRoot = new URL('../modules/ui/', import.meta.url)
  return {
    base: '/',
    plugins: [vue(), tailwindcss()],
    resolve: {
      dedupe: ['vue', '@lucide/vue'],
      // 源码联调显式启用；普通开发与发行构建继续使用锁定的包。
      alias: [
        { find: '@', replacement: fileURLToPath(new URL('./src', import.meta.url)) },
        ...(sourceUi
          ? [
              { find: /^@codex-proxy\/ui$/, replacement: fileURLToPath(new URL('src/index.ts', uiRoot)) },
              { find: '@codex-proxy/ui/theme', replacement: fileURLToPath(new URL('src/theme/index.ts', uiRoot)) },
              { find: '@codex-proxy/ui/styles.css', replacement: fileURLToPath(new URL('src/styles/index.css', uiRoot)) },
              { find: '@codex-proxy/ui/tailwind.css', replacement: fileURLToPath(new URL('src/styles/tailwind.css', uiRoot)) },
              { find: /^@codex-proxy\/ui\/(.+)$/, replacement: fileURLToPath(new URL('src/components/$1/index.ts', uiRoot)) },
            ]
          : []),
      ],
    },
    server: {
      port: 5173,
      fs: sourceUi ? { allow: [fileURLToPath(new URL('.', import.meta.url)), fileURLToPath(uiRoot)] } : undefined,
      proxy: {
        '/dev': {
          target: 'http://127.0.0.1:8080',
          changeOrigin: true,
          rewrite: path => path.replace(/^\/dev/, ''),
        },
      },
    },
    build: {
      outDir: sourceUi ? '.vite/source-dist' : 'dist',
      assetsDir: 'assets',
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
