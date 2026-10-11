<script setup lang="ts">
import type { getUsageRecordSummary } from '@/api'
import { ZCard, ZMotionIcon } from '@codex-proxy/ui'

import { Activity, Database, FileText, Timer } from '@lucide/vue'
import { computed } from 'vue'

const props = defineProps<{
  summary: Omit<Awaited<ReturnType<typeof getUsageRecordSummary>>, 'analytics'>
}>()

function averageLatencyDisplay(value: string) {
  return !value || value === '—' || value === '-' ? '0 ms' : value
}

const items = computed(() => [
  {
    key: 'requests',
    label: '成功请求',
    icon: Activity,
    value: props.summary.totalRequests,
    detail: '筛选范围内',
    tone: 'bg-cp-blue-container text-cp-blue-on-container',
  },
  {
    key: 'tokens',
    label: '总 Token',
    icon: FileText,
    value: props.summary.totalTokens,
    detail: `输入 ${props.summary.inputTokens} / 输出 ${props.summary.outputTokens}`,
    tone: 'bg-cp-green-container text-cp-green-on-container',
  },
  {
    key: 'cached',
    label: '缓存 Token',
    icon: Database,
    value: props.summary.cachedInputTokens,
    detail: '缓存读取命中',
    tone: 'bg-cp-orange-container text-cp-orange-on-container',
  },
  {
    key: 'latency',
    label: '平均耗时',
    icon: Timer,
    value: averageLatencyDisplay(props.summary.averageLatencyMs),
    detail: '成功请求平均值',
    tone: 'bg-cp-cyan-container text-cp-cyan-on-container',
  },
])
</script>

<template>
  <section class="mt-5 grid shrink-0 grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-4" aria-label="使用概览">
    <ZCard
      v-for="item in items"
      :key="item.key"
      as="article"
      padding="compact"
      class="grid min-h-23 grid-cols-[36px_minmax(0,1fr)] items-stretch gap-3"
    >
      <ZMotionIcon class="inline-flex size-9 shrink-0 items-center justify-center rounded-cp" :class="item.tone">
        <component :is="item.icon" class="size-4.5" />
      </ZMotionIcon>
      <div class="flex min-w-0 flex-col justify-between py-0.5">
        <span class="block text-cp-sm leading-none font-bold text-cp-text-quaternary">
          {{ item.label }}
        </span>
        <strong class="block truncate text-[22px] leading-none font-extrabold text-cp-text">
          {{ item.value }}
        </strong>
        <span class="block truncate text-cp-sm leading-none font-emphasis text-cp-text-secondary">
          {{ item.detail }}
        </span>
      </div>
    </ZCard>
  </section>
</template>
