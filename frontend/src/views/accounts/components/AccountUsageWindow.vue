<script setup lang="ts">
import type { AccountLocalUsage, AccountQuotaWindow } from '../constants'

import { computed } from 'vue'
import {
  quotaWindowBarClass,
  quotaWindowBarStyle,
  quotaWindowPercentTextClass,
} from '../constants'

type UsageWindowVariant = 'compact' | 'detail'
type UsageWindowMode = 'quota' | 'local' | 'unknown'

const props = withDefaults(
  defineProps<{
    window?: AccountQuotaWindow
    variant?: UsageWindowVariant
    showLocalValue?: boolean
  }>(),
  {
    variant: 'detail',
    showLocalValue: true,
  },
)

const isCompact = computed(() => props.variant === 'compact')
const localUsage = computed<AccountLocalUsage | null>(() => {
  const value = props.window?.localUsage
  if (!value || typeof value !== 'object' || Array.isArray(value))
    return null
  return value as AccountLocalUsage
})
const mode = computed<UsageWindowMode>(() => {
  if (localUsage.value)
    return 'local'
  if (props.window)
    return 'quota'
  return 'unknown'
})
const quotaValueVisible = computed(() =>
  typeof props.window?.usedPercent === 'number' && props.window.usedPercent > 0,
)

const rootClass = computed(() =>
  isCompact.value ? 'min-w-0' : 'rounded-lg bg-(--cp-bg-subtle) p-2',
)
const headerClass = computed(() =>
  isCompact.value
    ? 'mb-1 flex items-center justify-between gap-2 text-[11px] leading-none font-[720]'
    : 'flex items-center justify-between gap-3 text-[12px] font-[720]',
)
const labelClass = computed(() =>
  isCompact.value ? 'text-(--cp-text-muted)' : 'text-(--cp-text-secondary)',
)
const valueClass = computed(() =>
  isCompact.value
    ? 'text-[10px] leading-none font-[780]'
    : 'text-[12px] font-[780]',
)
const trackShapeClass = computed(() =>
  isCompact.value
    ? 'h-1.5 w-full overflow-hidden rounded-full'
    : 'h-2 overflow-hidden rounded-full',
)
const trackClass = computed(() => `${trackShapeClass.value} bg-(--cp-default-border)`)
const barStyle = computed(() => {
  if (!props.window)
    return undefined
  return quotaWindowBarStyle(props.window, isCompact.value ? '6px' : '8px')
})
const barClass = computed(() => props.window ? quotaWindowBarClass(props.window) : undefined)
const percentTextClass = computed(() =>
  props.window ? quotaWindowPercentTextClass(props.window) : undefined,
)
const localRequestLabel = '日请求'
const localRequestDisplay = computed(() => {
  const display = localUsage.value?.requestCountDisplay
  if (typeof display === 'string' && display.trim())
    return display.trim()
  const count = localUsage.value?.requestCount
  return typeof count === 'number' ? count.toLocaleString() : '0'
})
const localRequestValueVisible = computed(() => {
  const count = localUsage.value?.requestCount
  return props.showLocalValue && typeof count === 'number' && count > 0
})
const requestTimelineTitle = computed(() => `${localRequestLabel} ${localRequestDisplay.value} 次`)
const requestBars = computed(() => {
  const buckets = localUsage.value?.requestBuckets ?? []
  const hourMilliseconds = 60 * 60 * 1_000
  const currentHour = Math.floor(Date.now() / hourMilliseconds) * hourMilliseconds
  const bucketCounts = new Map(
    buckets.map((bucket) => {
      const bucketHour = Math.floor(new Date(bucket.bucketStart).getTime() / hourMilliseconds)
        * hourMilliseconds
      return [bucketHour, Math.max(0, bucket.requestCount)] as const
    }),
  )
  const slots = Array.from({ length: 24 }, (_, index) => {
    const startTime = currentHour - (23 - index) * hourMilliseconds
    return {
      startTime,
      requestCount: bucketCounts.get(startTime) ?? 0,
    }
  })
  const maximum = Math.max(1, ...slots.map(slot => slot.requestCount))
  const formatter = new Intl.DateTimeFormat('zh-CN', {
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  })
  return slots.map((slot) => {
    const start = new Date(slot.startTime)
    const end = new Date(slot.startTime + hourMilliseconds)
    return {
      key: start.toISOString(),
      requestCount: slot.requestCount,
      height: slot.requestCount === 0
        ? '0'
        : `${Math.max(25, Math.round(slot.requestCount / maximum * 100))}%`,
      title: `${formatter.format(start)}–${formatter.format(end)} · ${slot.requestCount} 次请求`,
    }
  })
})
</script>

<template>
  <div :class="rootClass">
    <template v-if="mode === 'quota' && window">
      <div :class="headerClass">
        <span class="min-w-0" :class="labelClass">
          {{ window.labelDisplay }}
        </span>
        <span
          v-if="quotaValueVisible"
          class="shrink-0 font-mono tabular-nums"
          :class="[valueClass, percentTextClass]"
        >
          {{ window.usedPercentDisplay }}
        </span>
      </div>
      <div
        :class="[trackClass, !isCompact ? 'mt-2' : undefined]"
        role="progressbar"
        :aria-label="window.labelDisplay"
        aria-valuemin="0"
        aria-valuemax="100"
        :aria-valuenow="window.usedPercent"
      >
        <div
          class="h-full rounded-full transition-[width,background-color] duration-200"
          :class="barClass"
          :style="barStyle"
        />
      </div>
      <div
        v-if="!isCompact && window.resetAtDisplay !== '—'"
        class="mt-3 text-[12px] font-[620] text-(--cp-text-secondary)"
      >
        重置时间: {{ window.resetAtDisplay }}
      </div>
    </template>

    <template v-else-if="mode === 'local'">
      <div :class="headerClass">
        <span class="min-w-0" :class="labelClass">
          {{ localRequestLabel }}
        </span>
        <strong
          v-if="localRequestValueVisible"
          class="shrink-0 font-mono tabular-nums text-(--cp-text-primary)"
          :class="valueClass"
        >
          {{ localRequestDisplay }}
        </strong>
      </div>
      <div
        class="flex items-stretch gap-px"
        :class="[trackShapeClass, !isCompact ? 'mt-2' : undefined]"
        role="img"
        :aria-label="requestTimelineTitle"
        :title="requestTimelineTitle"
      >
        <span
          v-for="bar in requestBars"
          :key="bar.key"
          class="relative min-w-0 flex-1 bg-(--cp-default-border)"
          :title="bar.title"
        >
          <span
            class="absolute inset-x-0 bottom-0 bg-(--cp-success)"
            :style="{ height: bar.height }"
          />
        </span>
      </div>
    </template>

    <div v-else :class="headerClass">
      <span class="min-w-0 text-(--cp-text-secondary)">额度待观测</span>
      <span class="shrink-0 font-mono text-(--cp-text-muted)" :class="valueClass">—</span>
    </div>
  </div>
</template>
