<script setup lang="ts">
import type { AccountQuotaWindow } from '../quota'
import {

  quotaWindowBarClass,
  quotaWindowBarStyle,
  quotaWindowLocalUsageDisplay,
  quotaWindowPercentTextClass,
  shouldShowQuotaWindowLocalUsage,
} from '../quota'

defineProps<{
  window: AccountQuotaWindow
}>()
</script>

<template>
  <div class="rounded-lg bg-(--cp-bg-subtle) p-2">
    <div class="flex items-center justify-between gap-3 text-[12px] font-[720]">
      <span class="text-(--cp-text-secondary)">{{ window.labelDisplay }}</span>
      <span class="flex shrink-0 items-baseline justify-end gap-1.5 font-mono tabular-nums">
        <span
          v-if="shouldShowQuotaWindowLocalUsage(window)"
          class="text-[12px] font-[680] text-(--cp-text-muted)"
        >
          {{ quotaWindowLocalUsageDisplay(window) }}
        </span>
        <span class="text-[12px] font-[780]" :class="quotaWindowPercentTextClass(window)">
          {{ window.usedPercentDisplay }}
        </span>
      </span>
    </div>
    <div class="mt-2 h-2 overflow-hidden rounded-full bg-(--cp-default-border)">
      <div
        class="h-full rounded-full transition-[width,background-color] duration-200"
        :class="quotaWindowBarClass(window)"
        :style="quotaWindowBarStyle(window)"
      />
    </div>
    <div
      class="mt-3 flex flex-wrap justify-between gap-x-3 gap-y-1 text-[12px] font-[620] text-(--cp-text-secondary)"
    >
      <span>重置时间: {{ window.resetAtDisplay }}</span>
      <span>窗口已用: {{ window.windowUsedDisplay }}</span>
    </div>
  </div>
</template>
