<script setup lang="ts">
import { CircleCheck, RefreshCw, ShieldAlert, TriangleAlert } from '@lucide/vue'

import type { AccountUsageItem, SemanticTone } from '../types'

defineProps<{
  accounts: AccountUsageItem[]
}>()

const scheduleStats = [
  { label: '单账号并发', value: '3' },
  { label: '可用槽位', value: '81' },
  { label: '剩余槽位', value: '54' },
]

const statusRows = [
  {
    label: '活跃账号',
    description: '可直接参与调度',
    value: '32',
    tone: 'success',
    icon: CircleCheck,
  },
  {
    label: '刷新中',
    description: '令牌自动刷新中',
    value: '2',
    tone: 'normal',
    icon: RefreshCw,
  },
  {
    label: '额度受限',
    description: '已触发或接近额度阈值',
    value: '6',
    tone: 'warning',
    icon: TriangleAlert,
  },
  {
    label: '不可用',
    description: '过期 4 · 禁用 3 · 封禁 0',
    value: '7',
    tone: 'danger',
    icon: ShieldAlert,
  },
] satisfies Array<{
  label: string
  description: string
  value: string
  tone: SemanticTone
  icon: typeof CircleCheck
}>

const rowToneClasses: Record<SemanticTone, string> = {
  normal: 'bg-[var(--cp-normal-bg)] text-[var(--cp-normal)]',
  info: 'bg-[var(--cp-info-bg)] text-[var(--cp-info)]',
  success: 'bg-[var(--cp-success-bg)] text-[var(--cp-success)]',
  warning: 'bg-[var(--cp-warning-bg)] text-[var(--cp-warning)]',
  danger: 'bg-[var(--cp-danger-bg)] text-[var(--cp-danger)]',
}

const valueToneClasses: Record<SemanticTone, string> = {
  normal: 'text-[var(--cp-normal-text)]',
  info: 'text-[var(--cp-info-text)]',
  success: 'text-[var(--cp-success-text)]',
  warning: 'text-[var(--cp-warning-text)]',
  danger: 'text-[var(--cp-danger-text)]',
}

const loadToneClasses: Record<SemanticTone, string> = {
  normal: 'bg-[var(--cp-normal)]',
  info: 'bg-[var(--cp-info)]',
  success: 'bg-[var(--cp-success)]',
  warning: 'bg-[var(--cp-warning)]',
  danger: 'bg-[var(--cp-danger)]',
}
</script>

<template>
  <article class="h-[450px] w-[1584px] rounded-[18px] bg-white shadow-[var(--cp-shadow-card)]">
    <div class="grid grid-cols-[360px_556px_556px] gap-7 px-7 pt-6">
      <section class="h-[402px] w-[360px]">
        <h2 class="m-0 text-[20px] leading-[1.15] font-[760] text-[var(--cp-text-primary)]">账号调度</h2>
        <p class="mt-[7px] mb-0 text-[13px] leading-[1.15] font-semibold text-[var(--cp-text-secondary)]">容量、并发与分配策略</p>

        <div class="mt-[31px] h-[122px] rounded-[14px] bg-[var(--cp-bg-subtle)] px-4 pt-[18px]">
          <span class="text-[12px] leading-[1.15] font-[650] text-[var(--cp-text-secondary)]">可调度容量</span>
          <div class="mt-[7px] flex items-end justify-between">
            <strong class="font-mono text-[32px] leading-[1.05] font-[760] tabular-nums text-[var(--cp-text-primary)]">81 / 135</strong>
            <span class="pb-[3px] text-[12px] leading-[1.15] font-[650] text-[var(--cp-text-secondary)]">60% 已分配</span>
          </div>
          <div class="mt-[17px] h-2.5 w-[328px] overflow-hidden rounded-full bg-[#E2E8F0]">
            <i class="block h-2.5 w-[197px] rounded-full bg-[var(--cp-success)]" />
          </div>
        </div>

        <div class="mt-4 grid h-[90px] grid-cols-3 rounded-[14px] bg-[var(--cp-bg-subtle)] px-4 pt-5">
          <div v-for="stat in scheduleStats" :key="stat.label">
            <span class="text-[12px] leading-[1.15] font-[650] text-[var(--cp-text-secondary)]">{{ stat.label }}</span>
            <strong class="mt-[13px] block font-mono text-[21px] leading-[1.1] font-[760] tabular-nums text-[var(--cp-text-primary)]">{{ stat.value }}</strong>
          </div>
        </div>

        <div class="mt-4 h-[82px] rounded-[14px] bg-[var(--cp-bg-subtle)] px-4 pt-[17px]">
          <span class="text-[12px] leading-[1.15] font-[650] text-[var(--cp-text-secondary)]">分配策略</span>
          <strong class="mt-[10px] block text-[17px] leading-[1.15] font-[650] text-[var(--cp-text-primary)]">最少使用优先</strong>
        </div>
      </section>

      <section class="w-[556px]">
        <h2 class="m-0 text-[20px] leading-[1.15] font-[760] text-[var(--cp-text-primary)]">活跃账号用量</h2>
        <p class="mt-[7px] mb-0 text-[13px] leading-[1.15] font-semibold text-[var(--cp-text-secondary)]">24h 请求排序</p>

        <div class="mt-[51px] grid h-[330px] w-[556px] gap-1.5 overflow-hidden">
          <article
            v-for="account in accounts"
            :key="account.name"
            class="grid h-[78px] w-[528px] grid-cols-[34px_10px_120px_6px_70px_30px_64px_2px_70px_4px_84px] items-center rounded-[14px] px-3.5"
            :class="account.name === 'Amy Ops' || account.name === 'Build Bot' ? 'bg-[var(--cp-bg-subtle)]' : 'bg-white'"
          >
            <span class="inline-flex size-[34px] items-center justify-center rounded-full" :class="rowToneClasses[account.tone]">
              {{ account.name[0] }}
            </span>
            <span class="col-start-3 grid">
              <span>
                <strong class="text-[14px] leading-[1.15] font-[650] text-[var(--cp-text-primary)]">{{ account.name }}</strong>
                <small class="ml-1.5 text-[11px] leading-[1.15] font-[650] text-[var(--cp-text-muted)]">{{ account.plan }}</small>
              </span>
              <span class="mt-[12px] text-[12px] leading-[1.15] font-semibold text-[var(--cp-text-secondary)]">{{ account.email }}</span>
            </span>
            <strong class="col-start-7 font-mono text-[14px] leading-[1.15] font-bold tabular-nums text-[var(--cp-text-primary)]">{{ account.requests }}</strong>
            <strong class="col-start-9 font-mono text-[14px] leading-[1.15] font-bold tabular-nums text-[var(--cp-text-primary)]">{{ account.tokens }}</strong>
            <span class="col-start-7 row-start-1 mt-11 text-[12px] leading-[1.15] font-semibold text-[var(--cp-text-secondary)]">{{ account.lastUsed }}</span>
            <span class="col-start-11 row-start-1 mt-[19px] block h-1.5 w-[84px] overflow-hidden rounded-full bg-[var(--cp-bg-muted)]">
              <i class="block h-1.5 rounded-full" :class="loadToneClasses[account.tone]" :style="{ width: `${account.loadWidth}px` }" />
            </span>
          </article>
        </div>
      </section>

      <section class="h-[402px] w-[556px]">
        <header class="flex h-[50px] items-start justify-between">
          <div>
            <h2 class="m-0 text-[20px] leading-[1.15] font-[760] text-[var(--cp-text-primary)]">账号状态</h2>
            <p class="mt-1 mb-0 text-[13px] leading-[1.15] font-[650] text-[var(--cp-text-secondary)]">账号池健康结构</p>
          </div>
          <div class="grid justify-items-end">
            <strong class="font-mono text-[24px] leading-[1.05] font-[760] tabular-nums text-[var(--cp-success-text)]">71.1%</strong>
            <span class="mt-0.5 text-[12px] leading-[1.15] font-bold text-[var(--cp-text-secondary)]">可用率</span>
          </div>
        </header>

        <div class="mt-[22px] h-[42px] w-[532px]">
          <div class="flex h-4 items-center justify-between">
            <span class="text-[12px] leading-[1.15] font-[650] text-[var(--cp-text-secondary)]">状态分布</span>
            <span class="text-[12px] leading-[1.15] font-[650] text-[var(--cp-danger-text)]">不可用 7</span>
          </div>
          <div class="mt-2.5 flex h-3 w-[532px] overflow-hidden rounded-full bg-[var(--cp-bg-muted)]">
            <i class="h-3 w-[372px] bg-[var(--cp-success)]" />
            <i class="h-3 w-11 bg-[var(--cp-normal)]" />
            <i class="h-3 w-14 bg-[var(--cp-warning)]" />
            <i class="h-3 w-[60px] bg-[var(--cp-danger)]" />
          </div>
        </div>

        <div class="mt-[26px] grid h-[262px] w-[532px] gap-2.5">
          <div
            v-for="row in statusRows"
            :key="row.label"
            class="grid h-[58px] grid-cols-[28px_14px_minmax(0,1fr)_76px] items-center rounded-[14px] bg-[var(--cp-bg-subtle)] px-3.5"
          >
            <span class="inline-flex size-7 items-center justify-center rounded-[9px]" :class="rowToneClasses[row.tone]">
              <component :is="row.icon" :size="16" />
            </span>
            <span class="col-start-3 grid gap-1">
              <strong class="text-[14px] leading-[1.15] font-[650] text-[var(--cp-text-primary)]">{{ row.label }}</strong>
              <span class="text-[12px] leading-[1.15] font-[650] text-[var(--cp-text-secondary)]">{{ row.description }}</span>
            </span>
            <strong class="col-start-4 text-right font-mono text-[17px] leading-[1.15] font-[760] tabular-nums" :class="valueToneClasses[row.tone]">{{ row.value }}</strong>
          </div>
        </div>
      </section>
    </div>
  </article>
</template>
