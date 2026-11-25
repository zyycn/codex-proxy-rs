<script setup lang="ts">
import type { DashboardHealthTimelinePoint } from '@/api'

import { computed } from 'vue'

import { useI18n } from 'vue-i18n'
import { formatHealthCount, healthEligibleRequestCount, healthObservedRequestCount, healthServiceFailureCount, healthStatusMeta } from './health'

const props = defineProps<{
  point: DashboardHealthTimelinePoint
}>()

const { t } = useI18n({ useScope: 'global' })

const status = computed(() => healthStatusMeta.value[props.point.status])
const eligibleRequests = computed(() => healthEligibleRequestCount(props.point))
const observedRequests = computed(() => healthObservedRequestCount(props.point))
// 结果标签、取值与配色集中维护，分段条与明细行都由此派生
const outcomeMeta = computed(() => [
  {
    label: t('common.status.success'),
    value: props.point.successRequests,
    dotClass: 'bg-z-success',
    valueClass: 'text-z-success-text',
  },
  {
    label: t('usage.health.point.serviceFailed'),
    value: healthServiceFailureCount(props.point),
    dotClass: 'bg-z-error',
    valueClass: 'text-z-error-text',
  },
  {
    label: t('usage.health.point.clientCancel'),
    value: props.point.cancelledRequests,
    dotClass: 'bg-z-orange-solid',
    valueClass: 'text-z-orange-text',
  },
  {
    label: t('usage.health.point.callerError'),
    value: props.point.callerErrorRequests,
    dotClass: 'bg-z-info',
    valueClass: 'text-z-info-text',
  },
])
const outcomeSegments = computed(() => {
  const total = observedRequests.value
  if (total === 0)
    return []

  return outcomeMeta.value
    .map(item => ({ label: item.label, value: item.value, className: item.dotClass }))
    .filter(item => item.value > 0)
    .map(item => ({
      ...item,
      percentage: (item.value / total) * 100,
    }))
})
const metricItems = outcomeMeta
</script>

<template>
  <section
    role="dialog"
    :aria-label="t('usage.health.point.requestHealthDetailsFor', { time: point.time })"
    class="overflow-hidden rounded-z-lg bg-z-bg-elevated"
  >
    <header class="flex items-center justify-between gap-3 bg-z-popover-header-bg px-3.5 py-3">
      <div class="flex min-w-0 items-center gap-2">
        <span class="size-2 shrink-0 rounded-full" :class="status.cellClass" />
        <strong class="font-mono text-z leading-none font-heavy text-z-text">
          {{ point.time }}
        </strong>
        <span
          class="inline-flex h-5 items-center rounded-full px-2 text-[10px] leading-none font-heavy"
          :class="status.badgeClass"
        >
          {{ status.label }}
        </span>
      </div>
      <span class="shrink-0 text-[10px] leading-none font-emphasis text-z-text-quaternary"> {{ t('usage.health.point.minutes') }} </span>
    </header>

    <div class="grid gap-3.5 px-3.5 py-3.5">
      <div class="flex items-end justify-between gap-4">
        <div>
          <p class="m-0 text-z-xs leading-none font-emphasis text-z-text-quaternary">
            {{ t('usage.health.point.validRequestAvailability') }}
          </p>
          <strong
            class="mt-2 block font-mono text-[25px] leading-none font-heavy tabular-nums"
            :class="status.valueClass"
          >
            {{ point.reliabilityDisplay }}
          </strong>
        </div>
        <div class="text-right">
          <span class="block text-[10px] leading-none font-emphasis text-z-text-quaternary"> {{ t('usage.health.point.validRequest') }} </span>
          <strong class="mt-2 block font-mono text-z-xl leading-none font-heavy tabular-nums text-z-text">
            {{ formatHealthCount(eligibleRequests) }}
          </strong>
        </div>
      </div>

      <div class="flex h-1.5 w-full overflow-hidden rounded-full bg-z-fill-tertiary">
        <span
          v-for="segment in outcomeSegments"
          :key="segment.label"
          class="h-full"
          :class="segment.className"
          :style="{ flexBasis: `${segment.percentage}%` }"
        />
      </div>

      <div class="grid grid-cols-2 gap-2">
        <div
          v-for="item in metricItems"
          :key="item.label"
          class="grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-2 rounded-z bg-z-fill-tertiary px-2.5 py-2.5"
        >
          <span class="size-1.5 rounded-full" :class="item.dotClass" />
          <span class="truncate text-[10px] leading-none font-emphasis text-z-text-secondary">
            {{ item.label }}
          </span>
          <strong class="font-mono text-z-sm leading-none font-heavy tabular-nums" :class="item.valueClass">
            {{ formatHealthCount(item.value) }}
          </strong>
        </div>
      </div>
    </div>
  </section>
</template>
