<script setup lang="ts">
import type { AccountQuotaForecast } from '@/api'
import { computed } from 'vue'

const props = defineProps<{ source: NonNullable<AccountQuotaForecast['source']>, methodDisplay: string }>()
const costCoverage = computed(() => {
  const total = props.source.knownCostCount + props.source.partialCostCount + props.source.unavailableCostCount
  return total > 0 ? `${(props.source.knownCostCount / total * 100).toFixed(1)}%` : '—'
})
const tokenRows = computed(() => [
  {
    label: '输入 Token',
    value: props.source.inputTokensDisplay,
    tone: 'bg-cp-green-container text-cp-green-on-container [html[data-theme=light]_&]:text-cp-green-text',
    markerTone: 'bg-cp-green-solid',
  },
  {
    label: '输出 Token',
    value: props.source.outputTokensDisplay,
    tone: 'bg-cp-orange-container text-cp-orange-on-container [html[data-theme=light]_&]:text-cp-orange-text',
    markerTone: 'bg-cp-orange-solid',
  },
  {
    label: '缓存命中',
    value: props.source.cachedTokensDisplay,
    tone: 'bg-cp-cyan-container text-cp-cyan-on-container [html[data-theme=light]_&]:text-cp-cyan-text',
    markerTone: 'bg-cp-cyan-solid',
  },
])
const timeRows = computed(() => [
  { label: '窗口开始', value: props.source.startAtDisplay },
  { label: '采样开始', value: props.source.sampleStartAtDisplay },
  { label: '采样截止 / 快照', value: props.source.observedAtDisplay },
  { label: '额度重置', value: props.source.resetAtDisplay },
])
</script>

<template>
  <section aria-label="采样依据" class="flex flex-col gap-5 rounded-cp-card bg-cp-fill-tertiary/70 p-4 md:justify-between [html[data-theme=light]_&]:bg-cp-fill-quaternary/70">
    <div class="grid gap-3">
      <div class="grid gap-1.5">
        <div class="flex items-baseline justify-between gap-3 text-cp-xs">
          <span class="text-cp-text-secondary">采样请求</span>
          <strong class="font-mono text-cp-sm font-heavy text-cp-text [html[data-theme=light]_&]:font-emphasis">{{ source.requestCountDisplay }}</strong>
        </div>
        <div class="flex flex-wrap items-baseline justify-between gap-2 text-cp-xs">
          <span class="text-cp-text-secondary">{{ methodDisplay }}</span>
          <span class="font-mono text-cp-text">{{ source.sampledPercentDisplay }}</span>
        </div>
      </div>
      <dl class="m-0 grid gap-2">
        <div
          v-for="row in tokenRows"
          :key="row.label"
          class="flex items-center justify-between gap-3 rounded-cp px-3 py-2.5 text-cp-xs [html[data-theme=light]_&]:bg-cp-bg-container"
          :class="row.tone"
        >
          <dt class="flex items-center gap-2 [html[data-theme=light]_&]:text-cp-text-secondary">
            <span class="hidden size-1.5 shrink-0 rounded-full [html[data-theme=light]_&]:block" :class="row.markerTone" aria-hidden="true" />
            {{ row.label }}
          </dt>
          <dd class="m-0 font-mono font-emphasis tabular-nums">
            {{ row.value }}
          </dd>
        </div>
      </dl>
    </div>
    <div class="grid gap-2">
      <div class="flex justify-between gap-3 text-cp-xs">
        <span class="text-cp-text-secondary">美元费用覆盖</span>
        <strong class="font-mono font-emphasis text-cp-text">{{ costCoverage }}</strong>
      </div>
      <div class="h-1 overflow-hidden rounded-full bg-cp-fill-secondary" role="img" :aria-label="`费用记录覆盖 ${costCoverage}`">
        <div class="h-full rounded-full bg-cp-success" :style="{ width: costCoverage === '—' ? '0%' : costCoverage }" />
      </div>
      <p class="m-0 text-cp-xs text-cp-text-tertiary">
        完整 {{ source.knownCostCountDisplay }} 次 · 部分 {{ source.partialCostCountDisplay }} 次 · 缺失 {{ source.unavailableCostCountDisplay }} 次
      </p>
      <p v-if="source.blockCount > 0" class="m-0 text-cp-xs text-cp-text-tertiary">
        {{ source.blockCount }} 个有效进度段 · 历史读数不等于独立样本
      </p>
      <p v-if="source.missingTokenCount > 0 || source.excludedRequestCount > 0 || source.pendingRequestCount > 0" class="m-0 text-cp-xs leading-relaxed text-cp-warning-text">
        Token 缺失 {{ source.missingTokenCount }} 次 · 未纳入 {{ source.excludedRequestCount }} 次 · 快照时未完成 {{ source.pendingRequestCount }} 次
      </p>
    </div>
    <dl class="m-0 grid gap-3 text-cp-xs">
      <div v-for="row in timeRows" :key="row.label" class="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
        <dt class="text-cp-text-secondary">
          {{ row.label }}
        </dt>
        <dd class="m-0 font-mono text-cp-text">
          {{ row.value }}
        </dd>
      </div>
    </dl>
  </section>
</template>
