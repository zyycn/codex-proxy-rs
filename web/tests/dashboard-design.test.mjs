import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')

async function source(path) {
  return readFile(resolve(root, path), 'utf8')
}

function assertTokens(sourceText, tokens) {
  for (const token of tokens) {
    assert.match(
      sourceText,
      new RegExp(token.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')),
      `Expected token: ${token}`,
    )
  }
}

function assertNoAbsoluteLayout(sourceText, label) {
  assert.doesNotMatch(
    sourceText,
    /\babsolute\b/,
    `${label} should use grid/flex/margin/padding flow layout, not absolute positioning`,
  )
}

test('dashboard page recreates the Pencil canvas with flow layout spacing', async () => {
  const view = await source('src/views/dashboard/DashboardView.vue')

  assertNoAbsoluteLayout(view, 'DashboardView')
  assertTokens(view, [
    'w-full min-w-[1180px] px-7 pt-[34px] pb-[34px]',
    'flex h-[68px] items-start justify-between',
    'mt-0 text-[34px] leading-[1.15] font-[800]',
    'mt-2.5 text-[15px] leading-[1.15] font-semibold',
    'mt-6 grid grid-cols-[repeat(4,minmax(0,1fr))] gap-6',
    'mt-6 grid grid-cols-[minmax(0,948fr)_minmax(0,608fr)] gap-7',
    'mt-6',
    'MetricCard',
    'RequestTrendCard',
    'ServiceStatusCard',
    'AccountOverviewCard',
    'EventLogCard',
  ])
})

test('admin layout keeps sidebar fixed and scrolls only the main region', async () => {
  const layout = await source('src/layout/index.vue')

  assertTokens(layout, [
    'ref(false)',
    ':collapsed="sidebarCollapsed"',
    '@toggle="sidebarCollapsed = !sidebarCollapsed"',
    'flex h-screen overflow-hidden bg-[var(--cp-bg-page)]',
    'cp-scrollbar h-screen min-w-0 flex-1 overflow-auto',
    'AppSidebar',
  ])
})

test('interactive controls use refined motion and respect reduced motion', async () => {
  const sidebar = await source('src/layout/components/AppSidebar.vue')
  const topbar = await source('src/layout/components/AppTopbar.vue')
  const global = await source('src/styles/base.css')

  assertTokens(sidebar, [
    "import { gsap } from 'gsap'",
    'gsap.to(sidebarEl.value',
    'width: targetWidth',
    'flexBasis: targetWidth',
    "ease: 'power3.out'",
    'animateSidebarLabels',
    'data-sidebar-toggle',
    'transition-[opacity,transform]',
    'duration-200',
    'hover:-translate-y-px',
    'active:translate-y-0',
  ])

  assertTokens(topbar, [
    "import { gsap } from 'gsap'",
    'animateAccountMenuEnter',
    'animateAccountMenuLeave',
    'gsap.fromTo(',
    "ease: 'power3.out'",
    "ease: 'power2.in'",
    'hover:-translate-y-px',
    'active:translate-y-0',
    'group-hover:rotate-180',
  ])

  assertTokens(global, [
    'button {\n  font-family: inherit;',
    '.cp-scrollbar',
    'scrollbar-color: var(--cp-scrollbar-thumb) transparent',
    'scrollbar-width: thin',
    '.cp-scrollbar::-webkit-scrollbar-thumb',
    'background-clip: content-box',
    '@media (prefers-reduced-motion: reduce)',
    'transition-duration: 0.01ms !important',
    'animation-duration: 0.01ms !important',
  ])

  assert.doesNotMatch(
    global,
    /button[\s\S]{0,80}font:\s*inherit/,
    'button reset must not override Tailwind text size, weight, and line-height utilities',
  )
})

test('topbar controls are normal flex items and only the popover floats', async () => {
  const topbar = await source('src/layout/components/AppTopbar.vue')

  assert.doesNotMatch(topbar, /absolute left-\[/)
  assertTokens(topbar, [
    'flex h-11 items-start gap-3.5',
    'ref(false)',
    '@click="accountMenuOpen = !accountMenuOpen"',
    'v-if="accountMenuOpen"',
    'data-account-trigger',
    'data-account-menu',
    'h-11 w-[170px]',
    'size-11',
    'h-11 w-[216px]',
    'relative',
    'absolute right-0 top-14',
    'h-[304px] w-80',
    'CalendarDays',
    'RefreshCw',
    'ChevronUp',
    'User',
    'ShieldCheck',
    'ScrollText',
    'LogOut',
    'MFA',
    '已开启',
    '会话',
    '1 台',
  ])
})

test('dashboard cards use grid and flex primitives with exact Pencil dimensions', async () => {
  const metric = await source('src/views/dashboard/components/MetricCard.vue')
  const trend = await source('src/views/dashboard/components/RequestTrendCard.vue')
  const service = await source('src/views/dashboard/components/ServiceStatusCard.vue')
  const account = await source('src/views/dashboard/components/AccountOverviewCard.vue')
  const events = await source('src/views/dashboard/components/EventLogCard.vue')

  for (const [label, file] of [
    ['MetricCard', metric],
    ['RequestTrendCard', trend],
    ['ServiceStatusCard', service],
    ['AccountOverviewCard', account],
    ['EventLogCard', events],
  ]) {
    assertNoAbsoluteLayout(file, label)
    assertTokens(file, [
      "import BaseCard from '../../../components/base/BaseCard.vue'",
      '<BaseCard',
      'as="article"',
      ':padded="false"',
    ])
  }

  assertTokens(metric, [
    'h-[154px] w-full',
    'px-6 pt-5',
    'flex items-start gap-3',
    'mt-2.5',
    'mt-[16.6px]',
    'grid h-[30px] w-full grid-cols-2',
    'inline-flex min-w-0 w-full items-center justify-start gap-3',
  ])
  assert.doesNotMatch(
    metric,
    /grid-cols-\[40px_82px_24px_48px_94px\]|justify-self-end|text-right/,
    'MetricCard detail strip must split into equal-width left-aligned halves',
  )

  assertTokens(trend, [
    'h-[380px] w-full',
    'px-7 pt-[22px]',
    'flex items-start justify-between',
    'h-[38px] w-[246px]',
    'mt-[19px] grid grid-cols-[minmax(0,1fr)_minmax(150px,180px)] gap-[30px]',
    'h-[268px] w-full',
    'preserveAspectRatio="none"',
    'grid-cols-[minmax(0,1fr)_8px]',
    'justify-self-end',
    'M0 150c70-14 96-20 136-38 48-24 74-20 108-58 42-42 78-22 116 6 44 32 82 18 124-10 44-24 82-16 120 30 18 22 34 12 46-6',
    'M0 70c80-2 122-8 170-6 56 2 116-14 166-8 62 6 94 18 142 2 48-16 108-2 172-10',
    'M0 44c72 4 116-8 174-4 58 2 110-6 166-2 66 8 118-8 174-4 60 6 100-4 136-8',
  ])
  assert.doesNotMatch(
    trend,
    /grid-cols-\[100px_8px\]/,
    'RequestTrendCard summary markers must align against the elastic summary panel width',
  )

  assertTokens(service, [
    'h-[380px] w-full',
    'px-7 pt-6',
    'mt-[23px] grid gap-2',
    'h-[50px] w-full',
  ])

  assertTokens(account, [
    'h-[450px] w-full',
    'grid grid-cols-[minmax(360px,360fr)_minmax(612px,612fr)_minmax(500px,500fr)] gap-7 px-7 pt-6',
    'h-[402px] w-full',
    'h-[330px] w-full',
    'w-[calc(100%-28px)]',
    'grid-cols-[34px_10px_minmax(220px,1fr)_6px_64px_2px_70px_4px_84px_6px]',
    'min-w-0',
    'block h-[14px] text-[12px] leading-[1.15] font-[650]',
    'mt-[12px] grid h-[34px] grid-cols-[minmax(0,1fr)_auto] items-start gap-4',
    'mt-[14px] text-[12px] leading-[1.15] font-[650]',
    'mt-[18px] h-2.5 w-full',
    'w-[60%]',
    'grid-cols-3 gap-4',
    '81 / 135',
    '60% 已分配',
    '最少使用优先',
    '71.1%',
    'basis-[71.1%]',
    'basis-[4.4%]',
    'basis-[13.3%]',
    'basis-[11.2%]',
  ])
  assert.doesNotMatch(
    account,
    /grid-cols-\[232px_96px\]|w-\[328px\]|w-\[197px\]|grid-cols-\[100px_16px_100px_16px_100px\]|className: 'col-start-5'|w-\[372px\]|w-11|w-14|w-\[60px\]/,
    'AccountOverviewCard schedule controls must stretch with the schedule panel width',
  )

  assertTokens(events, [
    'h-[350px] w-full',
    'px-7 pt-6',
    'flex items-start justify-between',
    'h-9 w-[206px]',
    "import BaseTable from '../../../components/base/BaseTable.vue'",
    'const eventLogColumns',
    "width: '11.5%'",
    "width: '17%'",
    "width: '24%'",
    ':columns="eventLogColumns"',
    ':rows="rows"',
    'table-class="min-w-full"',
    'header-row-class="h-10 rounded-xl bg-[var(--cp-bg-subtle)] text-[12px] leading-[1.15] font-bold text-[var(--cp-text-secondary)]"',
    'body-row-class="h-14 rounded-[10px] transition-[background-color,transform] duration-200 hover:-translate-y-px hover:bg-[var(--cp-bg-subtle)] active:translate-y-0"',
    'mt-[17px] flex h-[240px] w-full justify-between overflow-hidden',
    'levelToneClasses[rowTone(row)]',
    'statusToneClasses[rowTone(row)]',
    '请求 ID',
  ])

  assert.doesNotMatch(
    events,
    /grid-cols-\[[^\]]*fr/,
    'EventLogCard table columns must use fixed Pencil tracks so header and rows stay aligned',
  )
  assert.doesNotMatch(
    events,
    /w-\[3px\]/,
    'EventLogCard should rely on the base table scrollbar and must not render the extra right rail',
  )
})

test('dashboard data mirrors Pencil text values', async () => {
  const data = await source('src/views/dashboard/composables/useDashboard.ts')

  assertTokens(data, [
    '启用',
    '错误',
    '3,363',
    '< 3s',
    '成功率',
    '慢请求',
    'Amy Ops',
    'Team Codex',
    'Build Bot',
    'Reviewer',
    'team-codex@example.com',
    '24分钟前',
    '上游连接',
    '模型目录',
    '自动刷新',
    '事件记录',
    '客户端版本',
    'CloudCheck',
    'Boxes',
    'MonitorCheck',
    'req_01HW9K7QY2',
    'req_01HW9K6N4F',
    'req_01HW9K5Z7A',
    'req_01HW9K4C3Q',
    '/v1/chat/completions',
    'catalog',
  ])
})
