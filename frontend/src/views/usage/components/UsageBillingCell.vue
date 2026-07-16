<script setup lang="ts">
import { Info } from '@lucide/vue'
import { computed } from 'vue'

import BasePopover from '@/components/base/BasePopover.vue'
import { usageBilling, usageBillingText } from '../constants'

const props = defineProps<{
  record: any
}>()

const billing = computed(() => usageBilling(props.record))
const amountItems = computed(() => {
  const value = billing.value
  if (!value) return []

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
const billingItems = computed(() => {
  const value = billing.value
  if (!value) return []

  return [
    { label: '服务档位', value: value.serviceTierDisplay, tone: 'info' },
    { label: '倍率', value: value.multiplierDisplay, tone: 'info' },
    { label: '总费用', value: value.totalAmountDisplay, tone: 'success' },
    { label: '标准费用', value: value.standardAmountDisplay, tone: 'default' },
  ]
})

function itemValueClass(tone?: string, accent?: boolean) {
  if (tone === 'success') return 'text-(--cp-success-text)'
  if (tone === 'info' || accent) return 'text-(--cp-info-text)'
  return 'text-(--cp-text-primary)'
}
</script>

<template>
  <div class="flex items-center justify-end gap-1.5">
    <span class="font-mono text-[12px] font-[760] tabular-nums text-(--cp-success-text)">
      {{ usageBillingText(record) }}
    </span>

    <BasePopover
      v-if="billing"
      trigger="hover"
      placement="right"
      width="248px"
      panel-class="!p-3 text-(--cp-text-primary)"
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

      <div
        class="grid gap-2 text-[12px] leading-none [&_span:first-child]:whitespace-nowrap [&_span:last-child]:whitespace-nowrap"
      >
        <p class="m-0 font-[760] text-(--cp-text-primary)">计费明细</p>
        <div class="grid gap-1.5 text-(--cp-text-secondary)">
          <div v-for="item in amountItems" :key="item.label" class="flex justify-between gap-4">
            <span>{{ item.label }}</span>
            <span class="font-mono font-[760]" :class="itemValueClass(undefined, item.accent)">
              {{ item.value }}
            </span>
          </div>
        </div>
        <div
          class="mt-1 grid gap-1.5 border-t border-(--cp-divider-subtle) pt-2 text-(--cp-text-secondary)"
        >
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
