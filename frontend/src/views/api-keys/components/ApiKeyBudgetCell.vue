<script setup lang="ts">
import type { ApiKey } from '@/api'
import { computed } from 'vue'

const props = defineProps<{ apiKey: ApiKey }>()
const windows = computed(() => [
  { label: '日', used: props.apiKey.dailyUsedUsd, limit: props.apiKey.dailyLimitUsd, reset: props.apiKey.dailyResetsAt },
  { label: '周', used: props.apiKey.weeklyUsedUsd, limit: props.apiKey.weeklyLimitUsd, reset: props.apiKey.weeklyResetsAt },
])
function resetLabel(value: string | null) {
  return value ? `重置：${new Date(value).toLocaleString('zh-CN', { timeZone: 'Asia/Shanghai', hour12: false })}（北京时间）` : ''
}
function amount(value: string) {
  // 列表显示两位小数，悬停查看完整金额；记账与限额比较仍使用原始精度。
  return Number(value).toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })
}
</script>

<template>
  <div class="grid gap-1 text-xs tabular-nums">
    <div v-for="window in windows" :key="window.label" :title="`$${window.used} / ${Number(window.limit) === 0 ? '∞' : `$${window.limit}`} ${resetLabel(window.reset)}`" class="flex min-w-0 items-center gap-2">
      <span class="shrink-0 text-cp-text-tertiary">{{ window.label }}</span>
      <span class="truncate" :class="Number(window.limit) > 0 && Number(window.used) >= Number(window.limit) ? 'text-cp-error' : 'text-cp-text'">
        ${{ amount(window.used) }} / {{ Number(window.limit) === 0 ? '∞' : `$${amount(window.limit)}` }}
      </span>
    </div>
  </div>
</template>
