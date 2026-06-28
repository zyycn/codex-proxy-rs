<script setup lang="ts">
import { Info } from '@lucide/vue'
import { computed } from 'vue'

import BasePopover from '@/components/base/BasePopover.vue'
import { usageCostDetails, usageCostText } from '../constants'

const props = defineProps<{
  record: any
}>()

const costDetails = computed(() => usageCostDetails(props.record))
const mainCostItems = computed(() => {
  const details = costDetails.value
  if (!details) return []

  return [
    { label: '输入成本', value: details.inputCostDisplay, accent: false },
    { label: '输出成本', value: details.outputCostDisplay, accent: false },
    { label: '输入单价', value: details.inputPriceDisplay, accent: true },
    { label: '输出单价', value: details.outputPriceDisplay, accent: true },
    { label: '缓存读取成本', value: details.cacheReadCostDisplay, accent: false },
  ]
})
const billingItems = computed(() => {
  const details = costDetails.value
  if (!details) return []

  return [
    { label: '服务档位', value: details.serviceTierDisplay, tone: 'info' },
    { label: '倍率', value: details.multiplierDisplay, tone: 'info' },
    { label: '原始', value: details.originalCostDisplay, tone: 'default' },
    { label: '计费', value: details.billedCostDisplay, tone: 'success' },
  ]
})

function itemValueClass(tone?: string, accent?: boolean) {
  if (tone === 'success') return 'text-(--cp-success-text)'
  if (tone === 'info' || accent) return 'text-[#93c5fd]'
  return 'text-white'
}
</script>

<template>
  <div class="flex items-center justify-end gap-1.5">
    <span class="font-mono text-[12px] font-[760] tabular-nums text-(--cp-success-text)">
      {{ usageCostText(record) }}
    </span>

    <BasePopover
      v-if="costDetails"
      trigger="hover"
      placement="right"
      width="248px"
      panel-class="!bg-[#111827] !p-3 text-white shadow-(--cp-shadow-popover)"
    >
      <template #trigger>
        <button
          type="button"
          class="inline-flex size-4 items-center justify-center rounded-full bg-(--cp-info-bg) text-(--cp-info) outline-none hover:bg-(--cp-default-bg-hover)"
          aria-label="查看费用明细"
        >
          <Info class="size-3" />
        </button>
      </template>

      <div class="grid gap-2 text-[12px] leading-none [&_span:first-child]:whitespace-nowrap">
        <p class="m-0 font-[760] text-white">成本明细</p>
        <div class="grid gap-1.5 text-[#cbd5e1]">
          <div v-for="item in mainCostItems" :key="item.label" class="flex justify-between gap-4">
            <span>{{ item.label }}</span>
            <span class="font-mono font-[760]" :class="itemValueClass(undefined, item.accent)">
              {{ item.value }}
            </span>
          </div>
        </div>
        <div class="mt-1 grid gap-1.5 border-t border-white/12 pt-2 text-[#cbd5e1]">
          <div v-for="item in billingItems" :key="item.label" class="flex justify-between gap-4">
            <span>{{ item.label }}</span>
            <span class="font-mono font-[760]" :class="itemValueClass(item.tone)">
              {{ item.value }}
            </span>
          </div>
        </div>
      </div>
    </BasePopover>
  </div>
</template>
