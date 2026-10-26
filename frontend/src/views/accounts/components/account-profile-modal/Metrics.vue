<script setup lang="ts">
import type { AccountProfileStatisticsResponse } from '@/api'

import { ZSkeleton } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { formatCompactNumber, formatInteger } from '@/utils/format'

const props = defineProps<{
  profile: AccountProfileStatisticsResponse | null
  loading: boolean
}>()

const { t } = useI18n({ useScope: 'global' })

const metrics = computed(() => [
  {
    label: t('accounts.profile.metrics.cumulativeNumberOfTokens'),
    value: formatMetric(props.profile?.summary.totalTextTokens),
    title: formatExact(props.profile?.summary.totalTextTokens),
  },
  {
    label: t('accounts.profile.metrics.peakNumberOfTokens'),
    value: formatMetric(props.profile?.summary.peakTokens),
    title: formatExact(props.profile?.summary.peakTokens),
  },
  {
    label: t('accounts.profile.metrics.maximumChatDuration'),
    value: formatDuration(props.profile?.summary.longestTaskDurationMs),
  },
  {
    label: t('accounts.profile.metrics.currentConsecutiveDays'),
    value: formatDays(props.profile?.summary.currentStreakDays),
  },
  {
    label: t('accounts.profile.metrics.longestStreak'),
    value: formatDays(props.profile?.summary.longestStreakDays),
  },
])

function formatMetric(value: number | null | undefined) {
  return value == null
    ? '—'
    : formatCompactNumber(value)
}

function formatExact(value: number | null | undefined) {
  return value == null
    ? undefined
    : `${formatInteger(value)} Tokens`
}

function formatDays(value: number | null | undefined) {
  return value == null
    ? '—'
    : t('accounts.profile.metrics.dayCount', {
        integer: formatInteger(value),
      })
}

function formatDuration(value: number | null | undefined) {
  if (value == null)
    return '—'
  const totalMinutes = Math.floor(value / 60_000)
  const days = Math.floor(totalMinutes / (24 * 60))
  const hours = Math.floor((totalMinutes % (24 * 60)) / 60)
  const minutes = totalMinutes % 60
  if (days > 0) {
    return hours > 0
      ? t('accounts.profile.metrics.dH', { days, hours })
      : t('accounts.profile.metrics.elapsedDays', { days })
  }
  if (hours > 0) {
    return minutes > 0
      ? t('accounts.profile.metrics.hMin', { hours, minutes })
      : t('accounts.profile.metrics.elapsedHours', { hours })
  }
  if (totalMinutes > 0)
    return t('accounts.profile.metrics.min', { totalMinutes })
  return t('accounts.profile.metrics.durationSeconds', {
    value: Math.max(0, Math.floor(value / 1000)),
  })
}
</script>

<template>
  <section class="@container min-w-0 py-1" :aria-label="t('accounts.profile.metrics.cumulativeActivitySummary')" :aria-busy="loading">
    <dl class="grid grid-cols-2 gap-y-4 @[24rem]:grid-cols-3 @[34rem]:grid-cols-5">
      <div v-for="metric in metrics" :key="metric.label" class="min-w-0 px-2 text-center sm:px-3">
        <dd class="m-0 font-mono text-cp-lg leading-tight font-heavy whitespace-nowrap tabular-nums text-cp-text" :title="metric.title">
          <ZSkeleton v-if="loading && !profile" shape="text" class="mx-auto h-4 w-14" />
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
