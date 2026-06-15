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

  assert.doesNotMatch(
    source,
    /\babsolute\b|\bsticky\b|left-\[|top-\[|bottom-/,
    'sidebar should recreate Pencil spacing with flex/grid/margin/padding instead of positioned coordinates',
  )

  for (const token of [
    'w-[256px]',
    'basis-[256px]',
    'mt-[31px]',
    'ml-6',
    'grid h-[46px] grid-cols-[46px_minmax(0,1fr)] gap-2.5',
    'size-[46px]',
    'content-center overflow-hidden',
    'text-[17px] leading-[1.1] font-[720]',
    'mt-1 text-[12px] leading-[1.1] font-semibold',
    'mt-[35px] grid gap-3',
    "'ml-6 self-start'",
    'w-[208px]',
    'w-[88px]',
    'basis-[88px]',
    'w-[46px]',
    'h-[46px]',
    "'w-[208px] gap-3 pl-[22px]'",
    'font-bold text-[#111827]',
    'font-semibold text-[#64748B]',
    'mt-auto mb-8',
    "'mr-6 size-9 self-end bg-[#F8FAFC]'",
    'size-9',
  ]) {
    assert.match(source, new RegExp(token.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
  }

  assert.doesNotMatch(source, /w-\[280px\]|basis-\[280px\]|w-\[232px\]/)
})
