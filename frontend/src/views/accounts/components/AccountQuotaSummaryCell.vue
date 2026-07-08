<script setup lang="ts">
import { clamp } from 'es-toolkit'
import { computed } from 'vue'

const props = defineProps<{
  account: any
}>()

const FIVE_HOUR_WINDOW_SECONDS = 18_000
const WEEK_WINDOW_SECONDS = 604_800
const MONTH_WINDOW_SECONDS = 2_592_000

const quotaWindows = computed(() => props.account.quota.windows as any[])
const visibleQuotaWindows = computed(() => {
  const knownWindows = [...quotaWindows.value]
    .filter((window) => window.group === 'shortTerm' || window.group === 'monthly')
    .sort((a, b) => quotaWindowOrder(a) - quotaWindowOrder(b))

  return knownWindows.length > 0 ? knownWindows : quotaWindows.value
})
const summaryClass = computed(
  () =>
    `grid w-full max-w-40 min-w-0 gap-2 whitespace-normal py-0.5 ${
      visibleQuotaWindows.value.length === 1 ? 'min-h-13 content-center' : ''
    }`,
)

function quotaWindowPercent(window?: any) {
  return clamp(window?.usedPercent ?? 0, 0, 100)
}

function quotaWindowBarStyle(window?: any) {
  const percent = quotaWindowPercent(window)
  return {
    width: `${percent}%`,
    minWidth: percent > 0 ? '6px' : '0',
  }
}

function quotaWindowBarClass(window?: any) {
  if (window?.usedPercent === null || window?.usedPercent === undefined) {
    return 'bg-(--cp-default-border-hover)'
  }
  if (window.usedPercent >= 95) {
    return 'bg-(--cp-danger)'
  }
  if (window.usedPercent >= 80) {
    return 'bg-(--cp-warning)'
  }
  return 'bg-(--cp-success)'
}

function windowPercentDisplay(window?: any) {
  const display = window?.usedPercentDisplay
  return typeof display === 'string' && display.trim() ? display : '-'
}

function windowLocalUsageDisplay(window?: any) {
  const display = window?.localUsage?.totalTokensDisplay
  return typeof display === 'string' && display.trim() ? display : '-'
}

function shouldShowWindowLocalUsage(window?: any) {
  const totalTokens = window?.localUsage?.totalTokens
  return typeof totalTokens === 'number' && totalTokens > 0
}

function quotaWindowPercentTextClass(window?: any) {
  if (window?.usedPercent === null || window?.usedPercent === undefined) {
    return 'text-(--cp-text-muted)'
  }
  if (window.usedPercent >= 95) {
    return 'text-(--cp-danger-text)'
  }
  if (window.usedPercent >= 80) {
    return 'text-(--cp-warning-text)'
  }
  return 'text-(--cp-success-text)'
}

function quotaWindowOrder(window?: any) {
  const seconds = window?.windowSeconds
  if (quotaWindowMatches(seconds, FIVE_HOUR_WINDOW_SECONDS)) return 0
  if (quotaWindowMatches(seconds, WEEK_WINDOW_SECONDS)) return 1
  if (quotaWindowMatches(seconds, MONTH_WINDOW_SECONDS)) return 2
  return 3
}

function quotaWindowMatches(actual: unknown, expected: number) {
  return (
    typeof actual === 'number' &&
    Number.isFinite(actual) &&
    actual > 0 &&
    Math.abs(actual - expected) <= expected / 20
  )
}
</script>

<template>
  <div v-if="quotaWindows.length > 0" :class="summaryClass">
    <div v-for="window in visibleQuotaWindows" :key="window.key" class="min-w-0">
      <div class="mb-1 flex items-center justify-between gap-2 text-[10px] leading-none font-[760]">
        <span class="min-w-0 text-(--cp-text-secondary)">
          {{ window.labelDisplay }}
        </span>
        <span class="flex shrink-0 items-baseline justify-end gap-1.5 font-mono tabular-nums">
          <span
            v-if="shouldShowWindowLocalUsage(window)"
            class="text-[10px] leading-none font-[680] text-(--cp-text-muted)"
          >
            {{ windowLocalUsageDisplay(window) }}
          </span>
          <span
            class="text-[10px] leading-none font-[780]"
            :class="quotaWindowPercentTextClass(window)"
          >
            {{ windowPercentDisplay(window) }}
          </span>
        </span>
      </div>
      <div class="h-1 w-full overflow-hidden rounded-full bg-(--cp-default-border)">
        <div
          class="h-full rounded-full transition-[width,background-color] duration-200"
          :class="quotaWindowBarClass(window)"
          :style="quotaWindowBarStyle(window)"
        />
      </div>
    </div>
  </div>
  <span v-else class="inline-flex h-12 items-center text-(--cp-text-muted)">-</span>
</template>
