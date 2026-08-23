<script setup lang="ts">
import type { AccountQuotaWindow } from '../constants'

import { computed } from 'vue'
import { quotaWindowPresentation } from './AccountUsageWindow/presenter'

const props = defineProps<{
  label: string
  windows: AccountQuotaWindow[]
}>()

const windowItems = computed(() => props.windows.map(window => ({
  key: window.key,
  labelTooltip: window.labelDisplay,
  usedPercent: window.usedPercent,
  usedPercentDisplay: window.usedPercentDisplay,
  presentation: quotaWindowPresentation(window, '2px'),
})))

const trackGridStyle = computed(() => ({
  gridTemplateColumns: `repeat(${Math.max(windowItems.value.length, 1)}, minmax(0, 1fr))`,
}))
</script>

<template>
  <div class="flex min-w-0 flex-col">
    <div class="mb-1 min-w-0 truncate text-cp-xs leading-3.5 font-bold text-cp-text-quaternary">
      {{ label }}
    </div>

    <div class="grid min-w-0 gap-1" :style="trackGridStyle">
      <div
        v-for="item in windowItems"
        :key="item.key"
        class="h-0.75 w-full overflow-hidden rounded-full bg-cp-border-secondary"
        role="progressbar"
        :aria-label="item.labelTooltip"
        aria-valuemin="0"
        aria-valuemax="100"
        :aria-valuenow="item.usedPercent ?? undefined"
        :aria-valuetext="item.usedPercentDisplay"
      >
        <div
          class="h-full rounded-full transition-[width,background-color] duration-200"
          :class="item.presentation.barClass"
          :style="item.presentation.barStyle"
        />
      </div>
    </div>
  </div>
</template>
