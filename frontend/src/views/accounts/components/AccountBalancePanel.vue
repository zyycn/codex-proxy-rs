<script setup lang="ts">
import type { AccountBalance } from '@/api'
import { ZIconButton } from '@codex-proxy/ui'
import { RefreshCw } from '@lucide/vue'
import { computed } from 'vue'

const props = defineProps<{ balance: AccountBalance, loading: boolean, error: string, compact?: boolean }>()
const emit = defineEmits<{ refresh: [] }>()
const percentEntries = computed(() => props.balance.entries.filter(entry => entry.unit === '%' && entry.remaining !== null))
const summary = computed(() => props.balance.entries.find(entry => !entry.isValid)
  ?? props.balance.entries.find(entry => entry.remaining !== null && Number(entry.remaining) > 0)
  ?? props.balance.entries.find(entry => entry.remaining !== null || entry.total !== null || entry.unlimited)
  ?? props.balance.entries[0])
const summaryText = computed(() => {
  if (props.error && !summary.value)
    return props.error
  if (!summary.value)
    return '查询额度'
  if (!summary.value.isValid)
    return summary.value.invalidMessage || '上游 Key 不可用'
  if (percentEntries.value.length)
    return percentEntries.value.map(entry => `${entry.label}剩余 ${entry.remaining}%`).join(' · ')
  if (summary.value.unlimited)
    return '不限量'
  if (summary.value.remaining !== null)
    return `${summary.value.remaining} ${summary.value.unit}`
  if (summary.value.total !== null)
    return `${summary.value.used ?? '—'} / ${summary.value.total} ${summary.value.unit}`
  return summary.value.label
})
const updatedDisplay = computed(() => props.balance.observedAtDisplay ? `最近查询: ${props.balance.observedAtDisplay}` : '尚未查询')
</script>

<template>
  <div v-if="compact" class="flex min-w-0 items-center gap-1 text-cp-xs">
    <button
      type="button"
      class="flex min-w-0 cursor-pointer items-center gap-1 rounded-cp bg-transparent p-0 text-left font-mono tabular-nums text-cp-text-secondary hover:text-cp-primary focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-cp-primary disabled:cursor-wait"
      :class="{ 'text-cp-error': error }"
      :disabled="loading"
      :aria-busy="loading"
      :aria-label="`查询上游额度，${error || summaryText}`"
      :title="error || `${summaryText}，${updatedDisplay}，点击重新查询`"
      @click.stop="emit('refresh')"
    >
      <RefreshCw class="size-3 shrink-0" :class="{ 'animate-spin motion-reduce:animate-none': loading }" />
      <span v-if="percentEntries.length" class="grid min-w-0 gap-1">
        <span v-for="(entry, index) in percentEntries" :key="index" class="truncate">{{ entry.label }}剩余 {{ entry.remaining }}%</span>
      </span>
      <span v-else class="truncate">{{ summaryText }}</span>
      <span v-if="!percentEntries.length && balance.entries.length > 1" class="text-cp-text-quaternary">+{{ balance.entries.length - 1 }}</span>
    </button>
    <span v-if="error && summary" class="shrink-0 text-cp-error">查询失败</span>
  </div>
  <div v-else class="grid min-w-0 gap-3" aria-live="polite">
    <div class="flex items-center justify-between gap-3">
      <span class="text-cp-xs text-cp-text-secondary">{{ updatedDisplay }}</span>
      <ZIconButton size="small" aria-label="查询上游额度" :loading="loading" :disabled="loading" @click="emit('refresh')">
        <RefreshCw class="size-3.5" />
      </ZIconButton>
    </div>
    <p v-if="error" role="alert" class="m-0 text-cp-sm text-cp-error">{{ error }}</p>
    <p v-if="balance.entries.length === 0" class="m-0 text-cp-sm text-cp-text-secondary">尚未查询上游额度</p>
    <div v-for="(entry, index) in balance.entries" :key="index" class="grid gap-1.5 rounded-cp bg-cp-fill-quaternary px-3 py-2.5">
      <div class="flex flex-wrap items-baseline justify-between gap-2">
        <span class="text-cp-sm font-emphasis text-cp-text">{{ entry.label }}</span>
        <strong v-if="entry.unlimited || entry.remaining !== null || entry.total === null" class="font-mono text-cp-sm font-heavy tabular-nums text-cp-text wrap-anywhere">
          <template v-if="entry.unlimited">不限量</template>
          <template v-else-if="entry.remaining !== null && entry.unit === '%'">剩余 {{ entry.remaining }}%</template>
          <template v-else-if="entry.remaining !== null">剩余 {{ entry.remaining }} {{ entry.unit }}</template>
          <template v-else>未提供剩余额度</template>
        </strong>
      </div>
      <p v-if="!entry.isValid" class="m-0 text-cp-xs text-cp-error">{{ entry.invalidMessage }}</p>
      <p v-if="entry.unit !== '%' && (entry.used !== null || entry.total !== null)" class="m-0 font-mono text-cp-xs tabular-nums text-cp-text-secondary wrap-anywhere">
        已用 {{ entry.used ?? '—' }} / 总额 {{ entry.total ?? '—' }} {{ entry.unit }}
      </p>
      <div v-for="detail in entry.details" :key="detail.label" class="flex flex-wrap justify-between gap-2 text-cp-xs text-cp-text-secondary">
        <span>{{ detail.label }}</span>
        <span class="font-mono tabular-nums wrap-anywhere">{{ detail.amount }} {{ entry.unit }}</span>
      </div>
      <p v-if="entry.resetAtDisplay" class="m-0 text-cp-xs text-cp-text-secondary">重置: {{ entry.resetAtDisplay }}</p>
    </div>
  </div>
</template>
