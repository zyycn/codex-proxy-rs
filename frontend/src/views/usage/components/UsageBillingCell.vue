<script setup lang="ts">
import type { UsageDisplayRecord } from '../utils/records'

import { computed } from 'vue'
import { usageBilling, usageBillingText } from '../utils/records'
import UsageDetailPopover from './UsageDetailPopover.vue'

const props = defineProps<{
  record: UsageDisplayRecord
}>()

const billing = computed(() => usageBilling(props.record))
const billingItems = computed(() => {
  const value = billing.value
  if (!value)
    return []

  return [
    { label: '服务档位', value: value.serviceTierDisplay, tone: 'info' },
    { label: '倍率', value: value.multiplierDisplay, tone: 'info' },
    { label: '总费用', value: value.totalAmountDisplay, tone: 'success' },
    { label: '标准费用', value: value.standardAmountDisplay, tone: 'default' },
  ]
})

const amountItems = computed(() => {
  const value = billing.value
  if (!value)
    return []

  return [
    { label: '输入费用', value: value.inputAmountDisplay, accent: false },
    { label: '输出费用', value: value.outputAmountDisplay, accent: false },
    { label: '输入单价', value: value.inputPriceDisplay, accent: true },
    { label: '输出单价', value: value.outputPriceDisplay, accent: true },
    { label: '缓存读取费用', value: value.cacheReadAmountDisplay, accent: false },
    { label: '缓存写入费用', value: value.cacheWriteAmountDisplay, accent: false },
    { label: '缓存写入单价', value: value.cacheWritePriceDisplay, accent: true },
  ]
})

function itemValueClass(tone?: string, accent?: boolean) {
  if (tone === 'success')
    return 'text-cp-success-text'
  if (tone === 'info' || accent)
    return 'text-cp-info-text'
  return 'text-cp-text'
}
</script>

<template>
  <div class="flex items-center justify-end gap-1.5">
    <span class="font-mono text-cp-sm font-heavy tabular-nums text-cp-success-text">
      {{ usageBillingText(record) }}
    </span>

    <UsageDetailPopover v-if="billing" title="计费明细" trigger-label="查看费用明细">
      <div class="grid gap-1.5 text-cp-text-secondary">
        <div v-for="item in amountItems" :key="item.label" class="grid grid-cols-[auto_minmax(0,1fr)] items-center gap-4">
          <span class="whitespace-nowrap">{{ item.label }}</span>
          <span class="justify-self-end whitespace-nowrap font-mono font-heavy" :class="itemValueClass(undefined, item.accent)">
            {{ item.value }}
          </span>
        </div>
      </div>
      <div class="mt-1 grid gap-1.5 rounded-cp bg-cp-fill-quaternary p-2 text-cp-text-secondary">
        <div v-for="item in billingItems" :key="item.label" class="grid grid-cols-[auto_minmax(0,1fr)] items-center gap-4">
          <span class="whitespace-nowrap">{{ item.label }}</span>
          <span class="justify-self-end whitespace-nowrap font-mono font-heavy" :class="itemValueClass(item.tone)">
            {{ item.value }}
          </span>
        </div>
      </div>
    </UsageDetailPopover>
  </div>
</template>
