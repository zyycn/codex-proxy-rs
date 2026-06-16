<script setup lang="ts">
import { CircleCheck, RefreshCw, ShieldAlert, TriangleAlert } from '@lucide/vue'

import BaseCard from '../../../components/base/BaseCard.vue'
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
  normal: 'bg-(--cp-normal-bg) text-(--cp-normal)',
  info: 'bg-(--cp-info-bg) text-(--cp-info)',
  success: 'bg-(--cp-success-bg) text-(--cp-success)',
  warning: 'bg-(--cp-warning-bg) text-(--cp-warning)',
  danger: 'bg-(--cp-danger-bg) text-(--cp-danger)',
}

const valueToneClasses: Record<SemanticTone, string> = {
  normal: 'text-(--cp-normal-text)',
  info: 'text-(--cp-info-text)',
  success: 'text-(--cp-success-text)',
  warning: 'text-(--cp-warning-text)',
  danger: 'text-(--cp-danger-text)',
}

const loadToneClasses: Record<SemanticTone, string> = {
  normal: 'bg-(--cp-normal)',
  info: 'bg-(--cp-info)',
  success: 'bg-(--cp-success)',
  warning: 'bg-(--cp-warning)',
  danger: 'bg-(--cp-danger)',
}
</script>

<template>
  <BaseCard as="article" :padded="false" class="h-112.5 w-full">
    <div class="grid grid-cols-[minmax(360px,360fr)_minmax(612px,612fr)_minmax(500px,500fr)] gap-7 px-7 pt-6">
      <section class="h-100.5 w-full">
        <h2 class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)">账号调度</h2>
        <p class="mt-1.75 mb-0 text-[13px] leading-[1.15] font-semibold text-(--cp-text-secondary)">容量、并发与分配策略</p>

        <div class="mt-7.75 h-30.5 rounded-[14px] bg-(--cp-bg-subtle) px-4 pt-4.5">
          <span class="block h-3.5 text-xs leading-[1.15] font-[650] text-(--cp-text-secondary)">可调度容量</span>
          <div class="mt-3 grid h-8.5 grid-cols-[minmax(0,1fr)_auto] items-start gap-4">
            <strong class="font-mono text-[32px] leading-[1.05] font-[760] tabular-nums text-(--cp-text-primary)">81 / 135</strong>
            <span class="mt-3.5 text-xs leading-[1.15] font-[650] text-(--cp-text-secondary)">60% 已分配</span>
          </div>
          <div class="mt-4.5 h-2.5 w-full overflow-hidden rounded-full bg-slate-200">
            <i class="block h-2.5 w-3/5 rounded-full bg-(--cp-success)" />
          </div>
        </div>

        <div class="mt-4 grid h-22.5 grid-cols-3 gap-4 rounded-[14px] bg-(--cp-bg-subtle) px-4 pt-5">
          <div v-for="stat in scheduleStats" :key="stat.label">
            <span class="text-xs leading-[1.15] font-[650] text-(--cp-text-secondary)">{{ stat.label }}</span>
            <strong class="mt-3.25 block font-mono text-[21px] leading-[1.1] font-[760] tabular-nums text-(--cp-text-primary)">{{ stat.value }}</strong>
          </div>
        </div>

        <div class="mt-4 h-20.5 rounded-[14px] bg-(--cp-bg-subtle) px-4 pt-4.25">
          <span class="text-xs leading-[1.15] font-[650] text-(--cp-text-secondary)">分配策略</span>
          <strong class="mt-2.5 block text-[17px] leading-[1.15] font-[650] text-(--cp-text-primary)">最少使用优先</strong>
        </div>
      </section>

      <section class="w-full">
        <h2 class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)">活跃账号用量</h2>
        <p class="mt-1.75 mb-0 text-[13px] leading-[1.15] font-semibold text-(--cp-text-secondary)">24h 请求排序</p>

        <div class="mt-6.75 grid h-82.5 w-full gap-1.5 overflow-hidden">
          <article
            v-for="account in accounts"
            :key="account.name"
            class="grid h-19.5 w-[calc(100%-28px)] grid-cols-[34px_10px_minmax(220px,1fr)_6px_64px_2px_70px_4px_84px_6px] items-start rounded-[14px] px-3.5 transition-colors duration-200 hover:bg-(--cp-bg-subtle)"
            :class="account.name === 'Amy Ops' || account.name === 'Build Bot' ? 'bg-(--cp-bg-subtle)' : 'bg-white'"
          >
            <span class="mt-5.5 inline-flex size-8.5 items-center justify-center rounded-full" :class="rowToneClasses[account.tone]">
              {{ account.name[0] }}
            </span>
            <span class="col-start-3 row-start-1 mt-4.25 flex min-w-0 items-start">
                <strong class="text-sm leading-[1.15] font-[650] text-(--cp-text-primary)">{{ account.name }}</strong>
              <small class="ml-1.5 mt-0.75 text-[11px] leading-[1.15] font-[650] text-(--cp-text-muted)">{{ account.plan }}</small>
            </span>
            <span class="col-start-3 row-start-1 mt-12 min-w-0 truncate text-xs leading-[1.15] font-semibold text-(--cp-text-secondary)">{{ account.email }}</span>
            <strong class="col-start-5 row-start-1 mt-5 w-16 font-mono text-sm leading-[1.15] font-bold tabular-nums text-(--cp-text-primary)">{{ account.requests }}</strong>
            <strong class="col-start-7 row-start-1 mt-5 w-17.5 font-mono text-sm leading-[1.15] font-bold tabular-nums text-(--cp-text-primary)">{{ account.tokens }}</strong>
            <span class="col-start-5 row-start-1 mt-12 w-20 text-xs leading-[1.15] font-semibold text-(--cp-text-secondary)">{{ account.lastUsed }}</span>
            <span class="col-start-9 row-start-1 mt-9 block h-1.5 w-21 overflow-hidden rounded-full bg-(--cp-bg-muted)">
              <i class="block h-1.5 rounded-full" :class="loadToneClasses[account.tone]" :style="{ width: `${account.loadWidth}px` }" />
            </span>
          </article>
        </div>
      </section>

      <section class="h-100.5 w-full">
        <header class="flex h-12.5 items-start justify-between">
          <div>
            <h2 class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)">账号状态</h2>
            <p class="mt-1 mb-0 text-[13px] leading-[1.15] font-[650] text-(--cp-text-secondary)">账号池健康结构</p>
          </div>
          <div class="grid justify-items-end">
            <strong class="font-mono text-2xl leading-[1.05] font-[760] tabular-nums text-(--cp-success-text)">71.1%</strong>
            <span class="mt-0.5 text-xs leading-[1.15] font-bold text-(--cp-text-secondary)">可用率</span>
          </div>
        </header>

        <div class="mt-5.5 h-10.5 w-full">
          <div class="flex h-4 items-center justify-between">
            <span class="text-xs leading-[1.15] font-[650] text-(--cp-text-secondary)">状态分布</span>
            <span class="text-xs leading-[1.15] font-[650] text-(--cp-danger-text)">不可用 7</span>
          </div>
          <div class="mt-2.5 flex h-3 w-full overflow-hidden rounded-full bg-(--cp-bg-muted)">
            <i class="h-3 basis-[71.1%] bg-(--cp-success)" />
            <i class="h-3 basis-[4.4%] bg-(--cp-normal)" />
            <i class="h-3 basis-[13.3%] bg-(--cp-warning)" />
            <i class="h-3 basis-[11.2%] bg-(--cp-danger)" />
          </div>
        </div>

        <div class="mt-6.5 grid h-65.5 w-full gap-2.5">
          <div
            v-for="row in statusRows"
            :key="row.label"
            class="grid h-14.5 grid-cols-[28px_14px_minmax(0,1fr)_76px] items-center rounded-[14px] bg-(--cp-bg-subtle) px-3.5 transition-colors duration-200"
          >
            <span class="inline-flex size-7 items-center justify-center rounded-[9px]" :class="rowToneClasses[row.tone]">
              <component :is="row.icon" :size="16" />
            </span>
            <span class="col-start-3 grid gap-1">
              <strong class="text-sm leading-[1.15] font-[650] text-(--cp-text-primary)">{{ row.label }}</strong>
              <span class="text-xs leading-[1.15] font-[650] text-(--cp-text-secondary)">{{ row.description }}</span>
            </span>
            <strong class="col-start-4 text-right font-mono text-[17px] leading-[1.15] font-[760] tabular-nums" :class="valueToneClasses[row.tone]">{{ row.value }}</strong>
          </div>
        </div>
      </section>
    </div>
  </BaseCard>
</template>
