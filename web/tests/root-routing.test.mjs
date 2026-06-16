import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')

test('frontend is built and routed from the server root', async () => {
  const viteConfig = await readFile(resolve(root, 'vite.config.ts'), 'utf8')
  const router = await readFile(resolve(root, 'src/router/index.ts'), 'utf8')
  const html = await readFile(resolve(root, 'index.html'), 'utf8')

  assert.match(viteConfig, /base:\s*['"]\/['"]/)
  assert.doesNotMatch(viteConfig, /base:\s*['"]\/admin\/['"]/)
  assert.match(router, /createWebHistory\(['"]\/['"]\)/)
  assert.doesNotMatch(router, /createWebHistory\(['"]\/admin\/['"]\)/)
  assert.match(html, /href=['"]\/favicon\.svg['"]/)
  assert.doesNotMatch(html, /\/admin\/favicon\.svg/)
})
