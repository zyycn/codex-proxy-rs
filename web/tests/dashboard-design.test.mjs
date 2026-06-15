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
    'ml-7 w-[1584px] pt-[34px] pb-[60px]',
    'flex h-[68px] items-start justify-between',
    'mt-0 text-[34px] leading-[1.15] font-[800]',
    'mt-2.5 text-[15px] leading-[1.15] font-semibold',
    'mt-6 grid grid-cols-4 gap-6',
    'mt-6 grid grid-cols-[948px_608px] gap-7',
    'mt-6',
    'MetricCard',
    'RequestTrendCard',
    'ServiceStatusCard',
    'AccountOverviewCard',
    'EventLogCard',
  ])
})

test('topbar controls are normal flex items and only the popover floats', async () => {
  const topbar = await source('src/layout/components/AppTopbar.vue')

  assert.doesNotMatch(topbar, /absolute left-\[/)
  assertTokens(topbar, [
    'flex h-11 items-start gap-3.5',
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
  }

  assertTokens(metric, [
    'h-[154px] w-[378px]',
    'px-6 pt-5',
    'flex items-start gap-3',
    'mt-2.5',
    'mt-[16.6px]',
    'h-[30px] w-[330px]',
  ])

  assertTokens(trend, [
    'h-[380px] w-[948px]',
    'px-7 pt-[22px]',
    'flex items-start justify-between',
    'h-[38px] w-[246px]',
    'mt-7 grid grid-cols-[682px_180px] gap-[30px]',
    'h-[268px] w-[682px]',
    'M0 150c70-14 96-20 136-38 48-24 74-20 108-58 42-42 78-22 116 6 44 32 82 18 124-10 44-24 82-16 120 30 18 22 34 12 46-6',
    'M0 70c80-2 122-8 170-6 56 2 116-14 166-8 62 6 94 18 142 2 48-16 108-2 172-10',
    'M0 44c72 4 116-8 174-4 58 2 110-6 166-2 66 8 118-8 174-4 60 6 100-4 136-8',
  ])

  assertTokens(service, [
    'h-[380px] w-[608px]',
    'px-7 pt-6',
    'mt-[23px] grid gap-2',
    'h-[50px] w-[552px]',
  ])

  assertTokens(account, [
    'h-[450px] w-[1584px]',
    'grid grid-cols-[360px_556px_556px] gap-7 px-7 pt-6',
    'h-[402px] w-[360px]',
    'h-[330px] w-[556px]',
    'h-[402px] w-[556px]',
    '81 / 135',
    '60% 已分配',
    '最少使用优先',
    '71.1%',
  ])

  assertTokens(events, [
    'h-[350px] w-[1584px]',
    'px-7 pt-6',
    'flex items-start justify-between',
    'h-9 w-[206px]',
    'mt-[26px] h-10 w-[1528px]',
    'mt-4 flex h-[184px] w-[1528px] justify-between overflow-hidden',
    '请求 ID',
  ])
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
