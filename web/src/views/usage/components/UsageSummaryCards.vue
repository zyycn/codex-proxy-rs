<script setup lang="ts">
import { Activity, CircleAlert, FileText, Timer } from '@lucide/vue'

import { formatUsageMetric } from '../constants'

const props = defineProps<{
  summary: any
}>()

const items = [
  {
    key: 'requests',
    label: '总请求',
    icon: Activity,
    value: () => formatUsageMetric(props.summary.totalRequests),
    detail: () => '筛选范围内',
    tone: 'text-(--cp-info-text) bg-(--cp-info-bg)',
  },
  {
    key: 'tokens',
    label: '总 Token',
    icon: FileText,
    value: () => formatUsageMetric(props.summary.totalTokens),
    detail: () =>
      `输入 ${formatUsageMetric(props.summary.inputTokens)} / 输出 ${formatUsageMetric(
        props.summary.outputTokens,
      )}`,
    tone: 'text-(--cp-success-text) bg-(--cp-success-bg)',
  },
  {
    key: 'errors',
    label: '错误请求',
    icon: CircleAlert,
    value: () => formatUsageMetric(props.summary.errorRequests),
    detail: () => errorRateText(props.summary),
    tone: 'text-(--cp-danger-text) bg-(--cp-danger-bg)',
  },
  {
    key: 'latency',
    label: '平均耗时',
    icon: Timer,
    value: () => props.summary.averageLatencyMsDisplay || '—',
    detail: () => `缓存 ${formatUsageMetric(props.summary.cachedTokens)}`,
    tone: 'text-(--cp-normal-text) bg-(--cp-normal-bg)',
  },
]

function errorRateText(summary: any) {
  if (!summary.totalRequests) {
    return '错误率 0%'
  }

  return `错误率 ${((summary.errorRequests / summary.totalRequests) * 100).toFixed(1)}%`
}
</script>

<template>
  <section
    class="mt-5 grid shrink-0 grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-4"
    aria-label="使用概览"
  >
    <article
      v-for="item in items"
      :key="item.key"
      class="flex min-h-23 items-start gap-3 rounded-(--cp-card-radius) bg-(--cp-bg-surface) px-4 py-3 shadow-(--cp-shadow-card)"
    >
      <span
        class="mt-0.5 inline-flex size-9 shrink-0 items-center justify-center rounded-(--cp-input-radius-base)"
        :class="item.tone"
      >
        <component :is="item.icon" class="size-4.5" />
      </span>
      <div class="min-w-0">
        <span class="block text-[12px] leading-none font-bold text-(--cp-text-muted)">
          {{ item.label }}
        </span>
        <strong
          class="mt-2 block truncate text-[22px] leading-none font-[800] text-(--cp-text-primary)"
        >
          {{ item.value() }}
        </strong>
        <span
          class="mt-1.5 block truncate text-[12px] leading-none font-[650] text-(--cp-text-secondary)"
        >
          {{ item.detail() }}
        </span>
      </div>
    </article>
  </section>
</template>
