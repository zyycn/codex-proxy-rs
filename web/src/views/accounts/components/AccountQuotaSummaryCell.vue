<script setup lang="ts">
import { clamp } from 'es-toolkit'
import { computed } from 'vue'

const props = defineProps<{
  account: any
}>()

const quotaWindows = computed(() => props.account.quota.windows as any[])

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
</script>

<template>
  <div
    v-if="quotaWindows.length > 0"
    class="flex w-full max-w-31 min-w-0 flex-col gap-2 whitespace-normal py-1.5"
  >
    <div
      v-for="window in quotaWindows"
      :key="window.key"
      class="min-w-0 pb-2 last:border-b-0 last:pb-0"
    >
      <div
        class="mb-1.5 flex items-center justify-between gap-2 text-[11px] leading-none font-[760]"
      >
        <span class="truncate text-(--cp-text-secondary)">
          {{ window.labelDisplay }}
        </span>
        <span class="shrink-0 font-mono text-(--cp-text-primary)">
          {{ window.usedPercentDisplay }}
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
  <span v-else class="text-(--cp-text-muted)">-</span>
</template>
