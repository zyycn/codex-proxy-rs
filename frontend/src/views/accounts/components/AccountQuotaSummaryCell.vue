<script setup lang="ts">
import type { AccountRow } from '../quota'

import { computed } from 'vue'
import {

  quotaWindowBarClass,
  quotaWindowBarStyle,
  quotaWindowLocalUsageDisplay,
  quotaWindowPercentTextClass,
  shouldShowQuotaWindowLocalUsage,
  visibleSummaryQuotaWindows,
} from '../quota'

const props = defineProps<{
  account: AccountRow
}>()

const quotaWindows = computed(() => props.account.quota.windows)
const visibleQuotaWindows = computed(() => visibleSummaryQuotaWindows(quotaWindows.value))
const summaryClass = computed(
  () =>
    `grid w-full max-w-40 min-w-0 gap-2 whitespace-normal py-0.5 ${
      visibleQuotaWindows.value.length === 1 ? 'min-h-13 content-center' : ''
    }`,
)
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
            v-if="shouldShowQuotaWindowLocalUsage(window)"
            class="text-[10px] leading-none font-[680] text-(--cp-text-muted)"
          >
            {{ quotaWindowLocalUsageDisplay(window) }}
          </span>
          <span
            class="text-[10px] leading-none font-[780]"
            :class="quotaWindowPercentTextClass(window)"
          >
            {{ window.usedPercentDisplay }}
          </span>
        </span>
      </div>
      <div class="h-1 w-full overflow-hidden rounded-full bg-(--cp-default-border)">
        <div
          class="h-full rounded-full transition-[width,background-color] duration-200"
          :class="quotaWindowBarClass(window)"
          :style="quotaWindowBarStyle(window, '6px')"
        />
      </div>
    </div>
  </div>
  <span v-else class="inline-flex h-12 items-center text-(--cp-text-muted)">-</span>
</template>
