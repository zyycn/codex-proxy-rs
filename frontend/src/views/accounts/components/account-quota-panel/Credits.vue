<script setup lang="ts">
import type { AccountQuotaCredits } from '@/api'
import { computed } from 'vue'

const props = defineProps<{
  credits: AccountQuotaCredits | null
}>()

const integerFormatter = new Intl.NumberFormat('zh-CN')
const balanceDisplay = computed(() => {
  const balance = props.credits?.balance?.trim()
  const decimal = balance?.match(/^(-?)(\d+)(?:\.(\d+))?$/)
  if (!decimal)
    return null
  const [, sign, integer, fraction] = decimal
  // 点数可能包含高精度小数，整数分组与小数文本分别处理，避免转成浮点数。
  const grouped = `${sign}${integerFormatter.format(BigInt(integer!))}`
  return fraction ? `${grouped}.${fraction}` : grouped
})
const value = computed(() => {
  if (props.credits?.unlimited)
    return '无限'
  if (balanceDisplay.value !== null)
    return balanceDisplay.value
  if (props.credits && !props.credits.hasCredits)
    return '暂无可用点数'
  return '未提供余额'
})
const hasBalance = computed(() => !props.credits?.unlimited && balanceDisplay.value !== null)
</script>

<template>
  <div class="flex min-w-0 flex-wrap items-baseline justify-between gap-x-4 gap-y-1 rounded-cp bg-cp-fill-quaternary px-4 py-3.5" aria-label="额度点数">
    <span class="text-cp-sm font-heavy text-cp-text">额度点数</span>
    <span class="flex min-w-0 items-baseline gap-1.5">
      <strong
        class="min-w-0 text-right text-cp-sm font-heavy wrap-anywhere"
        :class="hasBalance ? 'font-mono tabular-nums text-cp-text' : 'text-cp-text-secondary'"
      >
        {{ value }}
      </strong>
      <span v-if="hasBalance" class="shrink-0 text-cp-xs text-cp-text-tertiary">点</span>
    </span>
  </div>
</template>
