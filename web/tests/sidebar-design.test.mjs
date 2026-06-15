import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const sidebarPath = resolve(root, 'src/layout/components/AppSidebar.vue')

test('sidebar uses Pencil design icons and brand copy', async () => {
  const source = await readFile(sidebarPath, 'utf8')

  for (const token of [
    'SquareTerminal',
    'LayoutDashboard',
    'Users',
    'KeyRound',
    'ScrollText',
    'ChartColumn',
    'Box',
    'Settings',
    'Radar',
    'PanelLeftClose',
    'Proxy RS · v0.1.0',
  ]) {
    assert.match(source, new RegExp(token.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
  }
})

test('sidebar locks core Pencil dimensions and coordinates', async () => {
  const source = await readFile(sidebarPath, 'utf8')

  for (const token of [
    'w-[280px]',
    'basis-[280px]',
    'left-[28px]',
    'top-[31px]',
    'size-[46px]',
    'left-[84px]',
    'top-[34px]',
    'top-[57px]',
    'left-6',
    'w-[232px]',
    'h-[46px]',
    'left-[22px]',
    'top-[13px]',
    'left-[54px]',
    'top-[15px]',
    'top-[112px]',
    'top-[170px]',
    'top-[228px]',
    'top-[286px]',
    'top-[344px]',
    'top-[402px]',
    'top-[460px]',
    'top-[518px]',
    'left-[220px]',
    'bottom-8',
    'size-9',
  ]) {
    assert.match(source, new RegExp(token.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
  }
})
