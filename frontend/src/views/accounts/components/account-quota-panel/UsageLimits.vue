<script setup lang="ts">
import type { AccountQuotaWindow } from '@/api'
import { computed } from 'vue'
import { quotaWindowPresentation } from '@/components/account/account-usage-window/presenter'
import { orderedPanelQuotaWindows } from '../../constants'

const props = defineProps<{
  windows: AccountQuotaWindow[]
}>()

const items = computed(() => orderedPanelQuotaWindows(props.windows)
  .filter(window => window.limitId === 'codex' || !window.limitId)
  .map((window) => {
    const remaining = window.usedPercent !== null && Number.isFinite(window.usedPercent)
      ? Math.round((100 - Math.min(100, Math.max(0, window.usedPercent))) * 10) / 10
      : null
    const presentation = quotaWindowPresentation(window, '0')

    return {
      key: window.key,
      label: window.labelDisplay,
      resetAtDisplay: window.resetAtDisplay,
      remaining,
      remainingDisplay: remaining === null ? '待观测' : `剩余 ${remaining}%`,
      barClass: presentation.barClass,
      percentTextClass: presentation.percentTextClass,
      barStyle: { width: `${remaining ?? 0}%` },
    }
  }))
</script>

<template>
  <section class="overflow-hidden rounded-cp bg-cp-fill-quaternary" aria-label="使用限额">
    <div
      v-for="item in items"
      :key="item.key"
      class="grid gap-3 px-4 py-4 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)] sm:items-center sm:gap-5"
    >
      <div class="min-w-0">
        <h3 class="m-0 text-cp-sm font-heavy text-cp-text">
          {{ item.label }}
        </h3>
        <p class="mt-1 mb-0 text-cp-xs leading-normal font-emphasis text-cp-text-secondary">
          重置：<span class="font-mono tabular-nums">{{ item.resetAtDisplay }}</span>
        </p>
      </div>
      <div class="flex min-w-0 items-center gap-3">
        <div
          class="h-1.5 min-w-0 flex-1 overflow-hidden rounded-full bg-cp-border-secondary"
          role="progressbar"
          :aria-label="`${item.label}剩余额度`"
          aria-valuemin="0"
          aria-valuemax="100"
          :aria-valuenow="item.remaining ?? undefined"
          :aria-valuetext="item.remainingDisplay"
        >
          <div
            class="h-full rounded-full transition-[width,background-color] duration-200 motion-reduce:transition-none"
            :class="item.barClass"
            :style="item.barStyle"
          />
        </div>
        <span class="shrink-0 text-cp-xs font-heavy tabular-nums" :class="item.percentTextClass">
          {{ item.remainingDisplay }}
        </span>
      </div>
    </div>
    <p v-if="items.length === 0" class="m-0 px-4 py-4 text-cp-sm font-emphasis text-cp-text-secondary">
      额度待观测
    </p>
  </section>
</template>
