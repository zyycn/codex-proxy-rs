<script setup lang="ts">
import type { getUsageRecordSummary } from '@/api'
import { Activity, Database, FileText, Timer } from '@lucide/vue'

import { computed } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseMotionIcon from '@/components/base/BaseMotionIcon.vue'

const props = defineProps<{
  summary: Awaited<ReturnType<typeof getUsageRecordSummary>>
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
    tone: 'bg-cp-blue-bg text-cp-blue-text-on-bg',
  },
  {
    key: 'tokens',
    label: '总 Token',
    icon: FileText,
    value: props.summary.totalTokens,
    detail: `输入 ${props.summary.inputTokens} / 输出 ${props.summary.outputTokens}`,
    tone: 'bg-cp-green-bg text-cp-green-text-on-bg',
  },
  {
    key: 'cached',
    label: '缓存 Token',
    icon: Database,
    value: props.summary.cachedTokens,
    detail: '缓存读取命中',
    tone: 'bg-cp-orange-bg text-cp-orange-text-on-bg',
  },
  {
    key: 'latency',
    label: '平均耗时',
    icon: Timer,
    value: averageLatencyDisplay(props.summary.averageLatencyMs),
    detail: '成功请求平均值',
    tone: 'bg-cp-cyan-bg text-cp-cyan-text-on-bg',
  },
])
</script>

<template>
  <section class="mt-5 grid shrink-0 grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-4" aria-label="使用概览">
    <BaseCard
      v-for="item in items"
      :key="item.key"
      as="article"
      padding="compact"
      class="grid min-h-23 grid-cols-[36px_minmax(0,1fr)] items-stretch gap-3"
    >
      <BaseMotionIcon class="inline-flex size-9 shrink-0 items-center justify-center rounded-cp" :class="item.tone">
        <component :is="item.icon" class="size-4.5" />
      </BaseMotionIcon>
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
    </BaseCard>
  </section>
</template>
