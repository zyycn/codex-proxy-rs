<script setup lang="ts">
import type { AccountProfileStatisticsResponse } from '@/api'
import { BaseSkeleton } from '@codex-proxy/ui'
import { computed } from 'vue'
import { formatCompactNumber, formatInteger } from '@/utils/format'

const props = defineProps<{
  profile: AccountProfileStatisticsResponse | null
  loading: boolean
}>()
const metrics = computed(() => [
  {
    label: '累计 Token 数',
    value: formatMetric(props.profile?.summary.totalTextTokens),
    title: formatExact(props.profile?.summary.totalTextTokens),
  },
  {
    label: '峰值 Token 数',
    value: formatMetric(props.profile?.summary.peakTokens),
    title: formatExact(props.profile?.summary.peakTokens),
  },
  {
    label: '最长聊天时长',
    value: formatDuration(props.profile?.summary.longestTaskDurationMs),
  },
  {
    label: '当前连续天数',
    value: formatDays(props.profile?.summary.currentStreakDays),
  },
  {
    label: '最长连续天数',
    value: formatDays(props.profile?.summary.longestStreakDays),
  },
])

function formatMetric(value: number | null | undefined) {
  return value == null ? '—' : formatCompactNumber(value)
}

function formatExact(value: number | null | undefined) {
  return value == null ? undefined : `${formatInteger(value)} Tokens`
}

function formatDays(value: number | null | undefined) {
  return value == null ? '—' : `${formatInteger(value)} 天`
}

function formatDuration(value: number | null | undefined) {
  if (value == null)
    return '—'
  const totalMinutes = Math.floor(value / 60_000)
  const days = Math.floor(totalMinutes / (24 * 60))
  const hours = Math.floor((totalMinutes % (24 * 60)) / 60)
  const minutes = totalMinutes % 60
  if (days > 0)
    return hours > 0 ? `${days} 天 ${hours} 小时` : `${days} 天`
  if (hours > 0)
    return minutes > 0 ? `${hours} 小时 ${minutes} 分` : `${hours} 小时`
  if (totalMinutes > 0)
    return `${totalMinutes} 分`
  return `${Math.max(0, Math.floor(value / 1000))} 秒`
}
</script>

<template>
  <section class="min-w-0 overflow-x-auto py-1" aria-label="累计活动摘要" :aria-busy="loading">
    <dl class="grid min-w-130 grid-cols-5">
      <div v-for="metric in metrics" :key="metric.label" class="min-w-0 px-2 text-center sm:px-3">
        <dd class="m-0 font-mono text-cp-lg leading-tight font-heavy tabular-nums text-cp-text" :title="metric.title">
          <BaseSkeleton v-if="loading && !profile" shape="text" class="mx-auto h-4 w-14" />
          <template v-else>
            {{ metric.value }}
          </template>
        </dd>
        <dt class="mt-1.5 text-cp-xs font-semibold text-cp-text-secondary">
          {{ metric.label }}
        </dt>
      </div>
    </dl>
  </section>
</template>
