<script setup lang="ts">
import { clamp } from 'es-toolkit'
import { computed } from 'vue'

const props = defineProps<{
  account: any
}>()

const quotaWindows = computed(() => props.account.quota.windows as any[])
const visibleQuotaWindows = computed(() => quotaWindows.value.slice(0, 2))
const summaryClass = computed(() =>
  visibleQuotaWindows.value.length === 1
    ? 'flex h-13 w-full max-w-31 min-w-0 flex-col justify-center whitespace-normal py-0.5'
    : 'grid h-13 w-full max-w-31 min-w-0 grid-rows-2 gap-1.5 whitespace-normal py-0.5',
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
</script>

<template>
  <div v-if="quotaWindows.length > 0" :class="summaryClass">
    <div v-for="window in visibleQuotaWindows" :key="window.key" class="min-w-0">
      <div class="mb-1 flex items-center justify-between gap-2 text-[11px] leading-none font-[760]">
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
  <span v-else class="inline-flex h-12 items-center text-(--cp-text-muted)">-</span>
</template>
