<script setup lang="ts">
import type { ApiKey } from '@/api'
import { ClipboardCheck } from '@lucide/vue'
import { computed } from 'vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'

const props = defineProps<{ apiKey: ApiKey }>()
const emit = defineEmits<{ reconcile: [apiKey: ApiKey] }>()
const windows = computed(() => [
  { label: '日', used: props.apiKey.dailyUsedUsd, limit: props.apiKey.dailyLimitUsd, reset: props.apiKey.dailyResetsAt },
  { label: '周', used: props.apiKey.weeklyUsedUsd, limit: props.apiKey.weeklyLimitUsd, reset: props.apiKey.weeklyResetsAt },
])
function resetLabel(value: string | null) {
  return value ? `重置：${new Date(value).toLocaleString('zh-CN', { timeZone: 'Asia/Shanghai', hour12: false })}（北京时间）` : ''
}
function amount(value: string) {
  return Number(value).toLocaleString('en-US', { maximumFractionDigits: 10 })
}
</script>

<template>
  <div class="grid gap-1 text-xs tabular-nums">
    <div v-for="window in windows" :key="window.label" :title="`$${window.used} / ${Number(window.limit) === 0 ? '不限' : `$${window.limit}`} ${resetLabel(window.reset)}`" class="flex min-w-0 items-center gap-2">
      <span class="shrink-0 text-cp-text-tertiary">{{ window.label }}</span>
      <span class="truncate" :class="Number(window.limit) > 0 && Number(window.used) >= Number(window.limit) ? 'text-cp-error' : 'text-cp-text'">
        ${{ amount(window.used) }} / {{ Number(window.limit) === 0 ? '不限' : `$${amount(window.limit)}` }}
      </span>
    </div>
    <div v-if="apiKey.unresolvedRequests" class="flex items-center gap-1 text-cp-warning">
      <span>待核账 {{ apiKey.unresolvedRequests }} 笔</span>
      <BaseIconButton variant="ghost" size="sm" label="费用核账" @click.stop="emit('reconcile', apiKey)">
        <ClipboardCheck class="size-3.5" />
      </BaseIconButton>
    </div>
  </div>
</template>
