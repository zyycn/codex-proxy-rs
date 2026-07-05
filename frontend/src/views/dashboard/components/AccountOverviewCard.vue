<script setup lang="ts">
import { clamp } from 'es-toolkit'
import { computed } from 'vue'
import { CircleCheck, RefreshCw, ShieldAlert, TriangleAlert } from '@lucide/vue'

import BaseCard from '../../../components/base/BaseCard.vue'
import BaseEmpty from '../../../components/base/BaseEmpty.vue'
import AccountPlanBadge from '../../accounts/components/AccountPlanBadge.vue'

const props = defineProps<{
  accounts: any[]
  pool?: any
  capacity?: any
  rotationStrategy?: string | null
}>()

const scheduleStats = computed(() => {
  const cap = props.capacity
  return [
    { label: '单账号并发', value: cap ? String(cap.maxConcurrentPerAccount) : '—' },
    { label: '总槽位', value: cap ? String(cap.totalSlots) : '—' },
    { label: '空闲槽位', value: cap ? String(cap.availableSlots) : '—' },
  ]
})

const usedPercent = computed(() => {
  const cap = props.capacity
  if (!cap || cap.totalSlots === 0) return 0
  return clamp(Math.round((cap.usedSlots / cap.totalSlots) * 100), 0, 100)
})

const usedRatio = computed(() => {
  const cap = props.capacity
  if (!cap) return '— / —'
  return `${cap.usedSlots} / ${cap.totalSlots}`
})

const strategyLabel = computed(() => {
  const s = props.rotationStrategy
  if (!s) return '—'
  const map: Record<string, string> = {
    least_used: '智能分配（推荐）',
    round_robin: '轮询',
    sticky: '粘滞',
  }
  return map[s] || s
})

const statusRows = computed(() => {
  const p = props.pool
  const active = p?.active ?? 0
  const refreshing = p?.refreshing ?? 0
  const quota = p?.quotaExhausted ?? 0
  const expired = p?.expired ?? 0
  const disabled = p?.disabled ?? 0
  const banned = p?.banned ?? 0
  const unavailable = expired + disabled + banned

  return [
    {
      label: '活跃账号',
      description: '可直接参与调度',
      value: String(active),
      tone: 'success',
      icon: CircleCheck,
    },
    {
      label: '刷新中',
      description: '令牌自动刷新中',
      value: String(refreshing),
      tone: 'normal',
      icon: RefreshCw,
    },
    {
      label: '额度受限',
      description: '已触发或接近额度阈值',
      value: String(quota),
      tone: 'warning',
      icon: TriangleAlert,
    },
    {
      label: '不可用',
      description: `过期 ${expired} · 禁用 ${disabled} · 封禁 ${banned}`,
      value: String(unavailable),
      tone: 'danger',
      icon: ShieldAlert,
    },
  ]
})

const availabilityRate = computed(() => {
  const p = props.pool
  if (!p || p.total === 0) return '0%'
  return `${((p.active / p.total) * 100).toFixed(1)}%`
})

const statusBars = computed(() => {
  const p = props.pool
  if (!p || p.total === 0) return []
  const active = (p.active / p.total) * 100
  const refreshing = (p.refreshing / p.total) * 100
  const quota = (p.quotaExhausted / p.total) * 100
  const unavailable = ((p.expired + p.disabled + p.banned) / p.total) * 100
  return [
    { pct: active, cls: 'bg-(--cp-success)' },
    { pct: refreshing, cls: 'bg-(--cp-normal)' },
    { pct: quota, cls: 'bg-(--cp-warning)' },
    { pct: unavailable, cls: 'bg-(--cp-danger)' },
  ].filter((b) => b.pct > 0)
})

const rowToneClasses: Record<string, string> = {
  normal: 'bg-(--cp-normal-bg) text-(--cp-normal)',
  info: 'bg-(--cp-info-bg) text-(--cp-info)',
  success: 'bg-(--cp-success-bg) text-(--cp-success)',
  warning: 'bg-(--cp-warning-bg) text-(--cp-warning)',
  danger: 'bg-(--cp-danger-bg) text-(--cp-danger)',
}

const valueToneClasses: Record<string, string> = {
  normal: 'text-(--cp-normal-text)',
  info: 'text-(--cp-info-text)',
  success: 'text-(--cp-success-text)',
  warning: 'text-(--cp-warning-text)',
  danger: 'text-(--cp-danger-text)',
}

const quotaToneClasses: Record<string, string> = {
  normal: 'bg-(--cp-normal)',
  info: 'bg-(--cp-info)',
  success: 'bg-(--cp-success)',
  warning: 'bg-(--cp-warning)',
  danger: 'bg-(--cp-danger)',
}
</script>

<template>
  <BaseCard as="article" :padded="false" class="w-full xl:h-112.5">
    <div
      class="grid px-4 pt-5 pb-6 lg:px-7 lg:pt-6 xl:grid-cols-[minmax(0,0.9fr)_minmax(0,1.28fr)_minmax(280px,0.9fr)] xl:gap-7 xl:pb-0"
    >
      <section class="min-w-0 w-full pb-6 xl:h-100.5 xl:pb-0">
        <h2 class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)">账号调度</h2>
        <p class="mt-1.75 mb-0 text-[13px] leading-[1.15] font-semibold text-(--cp-text-secondary)">
          容量、并发与分配策略
        </p>

        <div class="mt-7.75 grid h-30.5 content-between rounded-[14px] bg-(--cp-bg-subtle) p-4">
          <span class="block h-3.5 text-xs leading-[1.15] font-[650] text-(--cp-text-secondary)"
            >槽位占用</span
          >
          <div>
            <div class="grid h-8.5 grid-cols-[minmax(0,1fr)_auto] items-start gap-4">
              <strong
                class="font-mono text-[32px] leading-[1.05] font-[760] tabular-nums text-(--cp-text-primary)"
                >{{ usedRatio }}</strong
              >
              <span class="mt-3.5 text-xs leading-[1.15] font-[650] text-(--cp-text-secondary)"
                >{{ usedPercent }}% 已占用</span
              >
            </div>
            <div class="mt-4 h-2.5 w-full overflow-hidden rounded-full bg-(--cp-progress-track)">
              <i
                class="block h-2.5 rounded-full bg-(--cp-success)"
                :style="{ width: `${usedPercent}%` }"
              />
            </div>
          </div>
        </div>

        <div class="mt-4 grid h-22.5 grid-cols-3 gap-4 rounded-[14px] bg-(--cp-bg-subtle) p-4">
          <div v-for="stat in scheduleStats" :key="stat.label" class="grid content-between">
            <span class="text-xs leading-[1.15] font-[650] text-(--cp-text-secondary)">{{
              stat.label
            }}</span>
            <strong
              class="block font-mono text-[21px] leading-[1.1] font-[760] tabular-nums text-(--cp-text-primary)"
              >{{ stat.value }}</strong
            >
          </div>
        </div>

        <div class="mt-4 grid h-20.5 content-between rounded-[14px] bg-(--cp-bg-subtle) p-4">
          <span class="text-xs leading-[1.15] font-[650] text-(--cp-text-secondary)">分配策略</span>
          <strong class="block text-[17px] leading-[1.15] font-[650] text-(--cp-text-primary)">{{
            strategyLabel
          }}</strong>
        </div>
      </section>

      <section class="min-w-0 w-full">
        <h2 class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)">活跃账号用量</h2>
        <p class="mt-1.75 mb-0 text-[13px] leading-[1.15] font-semibold text-(--cp-text-secondary)">
          最近使用排序
        </p>

        <div
          class="mt-5 flex w-full flex-col gap-2 overflow-hidden xl:mt-6.75 xl:h-82.5 xl:gap-1.5"
        >
          <BaseEmpty
            v-if="accounts.length === 0"
            compact
            title="暂无账号用量"
            description="账号产生请求后会显示 24h 排序和 Token 负载。"
            class="min-h-40 place-content-center xl:h-full"
          />
          <template v-else>
            <article
              v-for="account in accounts"
              :key="account.name"
              class="grid w-full shrink-0 grid-cols-[34px_minmax(0,1fr)] gap-x-3 rounded-[14px] bg-(--cp-bg-subtle) px-3.5 py-3.5 hover:bg-(--cp-bg-muted) xl:h-19.5 xl:grid-cols-[34px_minmax(0,1.28fr)_minmax(132px,0.86fr)] xl:items-center xl:gap-x-3 xl:py-0"
            >
              <span
                class="inline-flex size-8.5 items-center justify-center rounded-full"
                :class="rowToneClasses[account.tone]"
              >
                {{ account.name[0] }}
              </span>
              <span class="flex min-w-0 max-w-full flex-col gap-1.5 xl:gap-4.25">
                <span class="flex min-w-0 max-w-full items-center gap-2">
                  <strong
                    class="min-w-0 truncate text-sm leading-[1.15] font-[650] text-(--cp-text-primary)"
                    :title="account.name"
                    >{{ account.name }}</strong
                  >
                  <AccountPlanBadge class="shrink-0" :plan-type="account.planType" />
                </span>
                <span
                  class="block min-w-0 max-w-full truncate text-xs leading-[1.15] font-semibold text-(--cp-text-secondary)"
                  :title="account.email"
                  >{{ account.email }}</span
                >
              </span>
              <span
                class="col-span-2 mt-3 grid grid-cols-3 gap-3 pl-11 xl:col-span-1 xl:mt-0 xl:grid-cols-[minmax(0,56px)_minmax(72px,1fr)] xl:gap-6 xl:pl-0"
              >
                <span class="grid min-w-0 gap-1.5 xl:gap-1">
                  <span
                    class="text-[11px] leading-[1.15] font-[650] text-(--cp-text-muted) xl:hidden"
                    >最近</span
                  >
                  <strong
                    class="hidden shrink-0 font-mono text-sm leading-[1.15] font-bold tabular-nums text-(--cp-text-primary) xl:block"
                    >{{ account.tokens }}</strong
                  >
                  <span
                    class="min-w-0 truncate text-xs leading-[1.15] font-semibold text-(--cp-text-secondary) xl:whitespace-nowrap"
                  >
                    {{ account.lastUsed }}
                  </span>
                </span>
                <span class="grid min-w-0 gap-1.5 xl:hidden">
                  <span
                    class="text-[11px] leading-[1.15] font-[650] text-(--cp-text-muted) xl:hidden"
                    >Token</span
                  >
                  <strong
                    class="w-full font-mono text-sm leading-[1.15] font-bold tabular-nums text-(--cp-text-primary)"
                    >{{ account.tokens }}</strong
                  >
                </span>
                <span class="grid min-w-0 gap-2">
                  <span
                    class="text-[11px] leading-[1.15] font-[650] text-(--cp-text-muted) xl:hidden"
                    >额度</span
                  >
                  <span
                    class="mt-1 block h-1.5 w-full overflow-hidden rounded-full bg-(--cp-bg-muted)"
                  >
                    <i
                      class="block h-1.5 rounded-full"
                      :class="quotaToneClasses[account.quotaTone]"
                      :style="{
                        width: `${account.quotaPercent}%`,
                        minWidth: account.quotaPercent > 0 ? '7px' : '0',
                      }"
                    />
                  </span>
                </span>
              </span>
            </article>
          </template>
        </div>
      </section>

      <section class="min-w-0 w-full pt-6 xl:h-100.5 xl:pt-0">
        <header class="flex h-12.5 items-start justify-between">
          <div>
            <h2 class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)">账号状态</h2>
            <p class="mt-1 mb-0 text-[13px] leading-[1.15] font-[650] text-(--cp-text-secondary)">
              账号池健康结构
            </p>
          </div>
          <div class="grid justify-items-end">
            <strong
              class="font-mono text-2xl leading-[1.05] font-[760] tabular-nums text-(--cp-success-text)"
              >{{ availabilityRate }}</strong
            >
            <span class="mt-0.5 text-xs leading-[1.15] font-bold text-(--cp-text-secondary)"
              >可用率</span
            >
          </div>
        </header>

        <div class="mt-5.5 h-10.5 w-full">
          <div class="flex h-4 items-center justify-between">
            <span class="text-xs leading-[1.15] font-[650] text-(--cp-text-secondary)"
              >状态分布</span
            >
            <span class="text-xs leading-[1.15] font-[650] text-(--cp-danger-text)"
              >不可用 {{ statusRows[3]?.value || '0' }}</span
            >
          </div>
          <div class="mt-2.5 flex h-3 w-full overflow-hidden rounded-full bg-(--cp-bg-muted)">
            <i
              v-for="(bar, bi) in statusBars"
              :key="bi"
              class="h-3"
              :class="bar.cls"
              :style="{ flexBasis: `${bar.pct}%` }"
            />
          </div>
        </div>

        <div class="mt-6.5 grid h-65.5 w-full gap-2.5">
          <div
            v-for="row in statusRows"
            :key="row.label"
            class="grid h-14.5 grid-cols-[28px_14px_minmax(0,1fr)_76px] items-center rounded-[14px] bg-(--cp-bg-subtle) px-3.5"
          >
            <span
              class="inline-flex size-7 items-center justify-center rounded-[9px]"
              :class="rowToneClasses[row.tone]"
            >
              <component :is="row.icon" :size="16" />
            </span>
            <span class="col-start-3 grid gap-1">
              <strong class="text-sm leading-[1.15] font-[650] text-(--cp-text-primary)">{{
                row.label
              }}</strong>
              <span class="text-xs leading-[1.15] font-[650] text-(--cp-text-secondary)">{{
                row.description
              }}</span>
            </span>
            <strong
              class="col-start-4 text-right font-mono text-[17px] leading-[1.15] font-[760] tabular-nums"
              :class="valueToneClasses[row.tone]"
              >{{ row.value }}</strong
            >
          </div>
        </div>
      </section>
    </div>
  </BaseCard>
</template>
