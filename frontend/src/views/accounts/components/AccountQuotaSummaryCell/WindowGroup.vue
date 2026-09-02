<script setup lang="ts">
import type { AccountQuotaWindow } from '../../constants'

import { computed } from 'vue'
import { quotaWindowPresentation } from '../AccountUsageWindow/presenter'

const props = withDefaults(defineProps<{
  label: string
  windows: AccountQuotaWindow[]
  showPercentage?: boolean
}>(), {
  showPercentage: true,
})

const windowItems = computed(() => props.windows.map(window => ({
  key: window.key,
  labelTooltip: window.labelDisplay,
  usedPercent: window.usedPercent,
  usedPercentDisplay: window.usedPercentDisplay,
  presentation: quotaWindowPresentation(window, '2px'),
})))
const highestUsageWindow = computed(() => props.windows.reduce((highest, window) => {
  if (typeof window.usedPercent !== 'number')
    return highest
  if (!highest || typeof highest.usedPercent !== 'number' || window.usedPercent > highest.usedPercent)
    return window
  return highest
}, undefined as AccountQuotaWindow | undefined))
const highestUsageDisplay = computed(() => highestUsageWindow.value?.usedPercentDisplay ?? '—')
const highestUsageTextClass = computed(() => highestUsageWindow.value
  ? quotaWindowPresentation(highestUsageWindow.value, '2px').percentTextClass
  : 'text-cp-text-quaternary')

const trackGridStyle = computed(() => ({
  gridTemplateColumns: `repeat(${Math.max(windowItems.value.length, 1)}, minmax(0, 1fr))`,
}))
</script>

<template>
  <div class="grid min-w-0 gap-1.5">
    <div class="flex min-w-0 items-baseline justify-between gap-1 text-[10px] leading-3 font-bold">
      <span class="min-w-0 truncate text-cp-text-quaternary" :title="label">
        {{ label }}
      </span>
      <strong
        v-if="showPercentage"
        class="shrink-0 font-mono font-heavy tabular-nums"
        :class="highestUsageTextClass"
      >
        {{ highestUsageDisplay }}
      </strong>
    </div>

    <div class="grid min-w-0 gap-1" :style="trackGridStyle">
      <div
        v-for="item in windowItems"
        :key="item.key"
        class="h-1 w-full overflow-hidden rounded-full bg-cp-border-secondary"
        role="progressbar"
        :aria-label="item.labelTooltip"
        aria-valuemin="0"
        aria-valuemax="100"
        :aria-valuenow="item.usedPercent ?? undefined"
        :aria-valuetext="item.usedPercentDisplay"
      >
        <div
          class="h-full rounded-full transition-[width,background-color] duration-200 motion-reduce:transition-none"
          :class="item.presentation.barClass"
          :style="item.presentation.barStyle"
        />
      </div>
    </div>
  </div>
</template>
