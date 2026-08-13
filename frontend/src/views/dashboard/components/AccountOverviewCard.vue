<script setup lang="ts">
import type { dashboardSnapshotView, MetricTone } from '../composables/presenter'
import { CircleCheck, CircleOff, RefreshCw, ShieldAlert, TriangleAlert } from '@lucide/vue'
import { clamp } from 'es-toolkit'

import { computed } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import AccountIdentityCell from '@/views/accounts/components/AccountIdentityCell.vue'
import AccountUsageWindow from '@/views/accounts/components/AccountUsageWindow.vue'
import { metricToneIconClasses, metricToneValueClasses } from '../constants'

type DashboardSnapshot = ReturnType<typeof dashboardSnapshotView>

const props = defineProps<{
  accounts: DashboardSnapshot['accountUsage']
  pool?: DashboardSnapshot['poolSummary']
  capacity?: DashboardSnapshot['capacityInfo']
  rotationStrategy?: string | null
}>()

const scheduleStats = computed(() => {
  const cap = props.capacity
  const display = (value: number | null | undefined) => value === null || value === undefined
    ? '—'
    : String(value)
  return [
    { label: '默认并发', value: display(cap?.maxConcurrentPerAccount) },
    { label: '总槽位', value: display(cap?.totalSlots) },
    { label: '空闲槽位', value: display(cap?.availableSlots) },
  ]
})

const usedPercent = computed(() => {
  const cap = props.capacity
  if (!cap || cap.totalSlots === 0 || cap.usedSlots === null || cap.usedSlots === undefined)
    return null
  return clamp(Math.round((cap.usedSlots / cap.totalSlots) * 100), 0, 100)
})

const usedRatio = computed(() => {
  const cap = props.capacity
  if (!cap || cap.totalSlots === null || cap.totalSlots === undefined)
    return '— / —'
  if (cap.usedSlots === null || cap.usedSlots === undefined)
    return `— / ${cap.totalSlots}`
  return `${cap.usedSlots} / ${cap.totalSlots}`
})

const occupancyLabel = computed(() => {
  if (usedPercent.value !== null)
    return `${usedPercent.value}% 已占用`
  const cap = props.capacity
  return cap && cap.totalSlots !== null && cap.totalSlots !== undefined
    ? ''
    : '暂无容量数据'
})

const strategyLabel = computed(() => {
  const s = props.rotationStrategy
  if (!s)
    return '—'
  const map: Record<string, string> = {
    smart: '智能调度（推荐）',
    quota_reset_priority: '额度重置优先',
    round_robin: '轮询',
    sticky: '粘滞',
  }
  return map[s] || s
})

function toneIconClass(tone: string) {
  return metricToneIconClasses[tone as MetricTone]
}

function toneValueClass(tone: string) {
  return metricToneValueClasses[tone as MetricTone]
}

const accountStatusCounts = computed(() => {
  const p = props.pool
  if (!p)
    return null
  const normal = p.normal ?? 0
  const quotaExhausted = p.quotaExhausted ?? 0
  const rateLimited = p.rateLimited ?? 0
  const disabled = p.disabled ?? 0
  const error = p.error ?? 0
  return {
    total: p.total,
    normal,
    quotaExhausted,
    rateLimited,
    disabled,
    error,
    unavailable: quotaExhausted + rateLimited + disabled + error,
  }
})

const statusRows = computed(() => {
  const p = props.pool
  const counts = accountStatusCounts.value
  if (!p || !counts) {
    return [
      { label: '正常', description: '暂无账号池观测', value: '—', tone: 'success', icon: CircleCheck },
      { label: '配额耗尽', description: '暂无账号池观测', value: '—', tone: 'warning', icon: TriangleAlert },
      { label: '限流中', description: '暂无账号池观测', value: '—', tone: 'normal', icon: RefreshCw },
      { label: '已停用', description: '暂无账号池观测', value: '—', tone: 'normal', icon: CircleOff },
      { label: '错误', description: '暂无账号池观测', value: '—', tone: 'danger', icon: ShieldAlert },
    ]
  }

  return [
    {
      label: '正常',
      description: '账号当前可用',
      value: String(counts.normal),
      tone: 'success',
      icon: CircleCheck,
    },
    {
      label: '配额耗尽',
      description: '账号额度已耗尽',
      value: String(counts.quotaExhausted),
      tone: 'warning',
      icon: TriangleAlert,
    },
    {
      label: '限流中',
      description: '账号请求暂时受限',
      value: String(counts.rateLimited),
      tone: 'normal',
      icon: RefreshCw,
    },
    {
      label: '已停用',
      description: '账号已停用',
      value: String(counts.disabled),
      tone: 'normal',
      icon: CircleOff,
    },
    {
      label: '错误',
      description: '账号状态异常',
      value: String(counts.error),
      tone: 'danger',
      icon: ShieldAlert,
    },
  ]
})

const normalRate = computed(() => {
  const p = props.pool
  if (!p || p.total === 0)
    return '—'
  return `${((p.normal / p.total) * 100).toFixed(1)}%`
})

const statusBars = computed(() => {
  const counts = accountStatusCounts.value
  if (!counts || counts.total === 0)
    return []
  return [
    { pct: (counts.normal / counts.total) * 100, cls: 'bg-(--cp-success)' },
    { pct: (counts.quotaExhausted / counts.total) * 100, cls: 'bg-(--cp-warning)' },
    { pct: (counts.rateLimited / counts.total) * 100, cls: 'bg-(--cp-normal)' },
    { pct: (counts.disabled / counts.total) * 100, cls: 'bg-(--cp-text-secondary)' },
    { pct: (counts.error / counts.total) * 100, cls: 'bg-(--cp-danger)' },
  ].filter(b => b.pct > 0)
})
</script>

<template>
  <BaseCard as="article" class="w-full xl:h-112.5">
    <div
      class="grid xl:grid-cols-[minmax(0,0.9fr)_minmax(0,1.28fr)_minmax(280px,0.9fr)] xl:gap-7"
    >
      <section class="min-w-0 w-full pb-6 xl:h-100.5 xl:pb-0">
        <h2 class="m-0 text-xl leading-[1.15] font-heavy text-(--cp-text-primary)">
          账号调度
        </h2>
        <p class="mt-1.75 mb-0 text-[13px] leading-[1.15] font-semibold text-(--cp-text-secondary)">
          容量、并发与分配策略
        </p>

        <div class="mt-6.75 grid gap-4 xl:h-82.5 xl:grid-rows-[122px_90px_minmax(0,1fr)]">
          <div class="grid h-30.5 content-between rounded-[14px] bg-(--cp-bg-subtle) p-4 xl:h-auto">
            <span class="block h-3.5 text-xs leading-[1.15] font-emphasis text-(--cp-text-secondary)">槽位占用</span>
            <div>
              <div class="grid h-8.5 grid-cols-[minmax(0,1fr)_auto] items-start gap-4">
                <strong
                  class="font-mono text-[32px] leading-[1.05] font-heavy tabular-nums text-(--cp-text-primary)"
                >{{ usedRatio }}</strong>
                <span class="mt-3.5 text-xs leading-[1.15] font-emphasis text-(--cp-text-secondary)">
                  {{ occupancyLabel }}
                </span>
              </div>
              <div class="mt-4 h-2.5 w-full overflow-hidden rounded-full bg-(--cp-progress-track)">
                <i
                  class="block h-2.5 rounded-full bg-(--cp-success)"
                  :style="{ width: usedPercent === null ? '0' : `${usedPercent}%` }"
                />
              </div>
            </div>
          </div>

          <div
            class="grid h-22.5 grid-cols-3 gap-4 rounded-[14px] bg-(--cp-bg-subtle) p-4 xl:h-auto"
          >
            <div v-for="stat in scheduleStats" :key="stat.label" class="grid content-between">
              <span class="text-xs leading-[1.15] font-emphasis text-(--cp-text-secondary)">{{
                stat.label
              }}</span>
              <strong
                class="block font-mono text-[21px] leading-[1.1] font-heavy tabular-nums text-(--cp-text-primary)"
              >{{ stat.value }}</strong>
            </div>
          </div>

          <div class="grid h-20.5 content-between rounded-[14px] bg-(--cp-bg-subtle) p-4 xl:h-auto">
            <span class="text-xs leading-[1.15] font-emphasis text-(--cp-text-secondary)">分配策略</span>
            <strong class="block text-[17px] leading-[1.15] font-emphasis text-(--cp-text-primary)">{{
              strategyLabel
            }}</strong>
          </div>
        </div>
      </section>

      <section class="min-w-0 w-full">
        <h2 class="m-0 text-xl leading-[1.15] font-heavy text-(--cp-text-primary)">
          活跃账号用量
        </h2>
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
            description="暂无账号请求记录"
            class="min-h-40 place-content-center xl:h-full"
          />
          <template v-else>
            <article
              v-for="account in accounts"
              :key="account.id"
              class="grid w-full shrink-0 grid-cols-1 gap-3 rounded-xl bg-(--cp-bg-subtle) px-3.5 py-3.5 transition-colors hover:bg-(--cp-bg-muted) xl:h-19.5 xl:grid-cols-[minmax(0,1fr)_minmax(70px,0.36fr)_minmax(74px,0.38fr)_minmax(82px,0.46fr)] xl:items-center xl:gap-4 xl:py-0"
            >
              <AccountIdentityCell
                :account="account"
                show-plan
                title-mode="email"
                meta-position="secondary"
                class="min-w-0"
              >
                <template #meta>
                  <ProviderIconGroup
                    :provider="account.provider"
                    :authentication-kind="account.authenticationKind"
                    size="sm"
                  />
                </template>
              </AccountIdentityCell>

              <span
                class="grid min-w-0 grid-cols-[minmax(0,0.82fr)_minmax(0,0.64fr)_minmax(104px,1fr)] items-start gap-3 xl:contents"
              >
                <span class="grid min-w-0 gap-1">
                  <span class="text-[11px] leading-none font-bold text-(--cp-text-muted)">
                    {{ account.metricLabel }}
                  </span>
                  <strong
                    class="font-mono text-sm leading-[1.15] font-heavy tabular-nums text-(--cp-text-primary)"
                  >{{ account.metricValue }}</strong>
                </span>

                <span class="grid min-w-0 gap-1">
                  <span class="text-[11px] leading-none font-bold text-(--cp-text-muted)">最近</span>
                  <span
                    class="min-w-0 truncate text-xs leading-[1.15] font-bold text-(--cp-text-secondary) xl:whitespace-nowrap"
                  >
                    {{ account.lastUsed }}
                  </span>
                </span>

                <AccountUsageWindow
                  :window="account.usageWindow"
                  :show-local-value="false"
                  variant="metric"
                />
              </span>
            </article>
          </template>
        </div>
      </section>

      <section class="min-w-0 w-full pt-6 xl:h-100.5 xl:pt-0">
        <header class="flex h-12.5 items-start justify-between">
          <div>
            <h2 class="m-0 text-xl leading-[1.15] font-heavy text-(--cp-text-primary)">
              账号状态
            </h2>
            <p class="mt-1 mb-0 text-[13px] leading-[1.15] font-emphasis text-(--cp-text-secondary)">
              账号池健康结构
            </p>
          </div>
          <div class="grid justify-items-end">
            <strong
              class="font-mono text-2xl leading-[1.05] font-heavy tabular-nums text-(--cp-success-text)"
            >{{ normalRate }}</strong>
            <span class="mt-0.5 text-xs leading-[1.15] font-bold text-(--cp-text-secondary)">正常率</span>
          </div>
        </header>

        <div class="mt-5.5 h-10.5 w-full">
          <div class="flex h-4 items-center justify-between">
            <span class="text-xs leading-[1.15] font-emphasis text-(--cp-text-secondary)">状态分布</span>
            <span class="text-xs leading-[1.15] font-emphasis text-(--cp-danger-text)">不可用 {{ accountStatusCounts?.unavailable ?? '—' }}</span>
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

        <div class="mt-5 grid h-67 w-full grid-rows-5 gap-2">
          <div
            v-for="row in statusRows"
            :key="row.label"
            class="grid min-h-0 grid-cols-[28px_14px_minmax(0,1fr)_76px] items-center rounded-[14px] bg-(--cp-bg-subtle) px-3.5"
          >
            <span
              class="inline-flex size-7 items-center justify-center rounded-[9px]"
              :class="toneIconClass(row.tone)"
            >
              <component :is="row.icon" :size="16" />
            </span>
            <span class="col-start-3 grid gap-1">
              <strong class="text-sm leading-[1.15] font-emphasis text-(--cp-text-primary)">{{
                row.label
              }}</strong>
              <span class="text-xs leading-[1.15] font-emphasis text-(--cp-text-secondary)">{{
                row.description
              }}</span>
            </span>
            <strong
              class="col-start-4 text-right font-mono text-[17px] leading-[1.15] font-heavy tabular-nums"
              :class="toneValueClass(row.tone)"
            >{{ row.value }}</strong>
          </div>
        </div>
      </section>
    </div>
  </BaseCard>
</template>
