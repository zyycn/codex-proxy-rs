<script setup lang="ts">
import { Activity, Database, FileText, Timer } from '@lucide/vue'

const props = defineProps<{
  summary: any
}>()

const items = [
  {
    key: 'requests',
    label: '成功请求',
    icon: Activity,
    value: () => props.summary.totalRequests,
    detail: () => '筛选范围内',
    tone: 'text-(--cp-info-text) bg-(--cp-info-bg)',
  },
  {
    key: 'tokens',
    label: '总 Token',
    icon: FileText,
    value: () => props.summary.totalTokens,
    detail: () => `输入 ${props.summary.inputTokens} / 输出 ${props.summary.outputTokens}`,
    tone: 'text-(--cp-success-text) bg-(--cp-success-bg)',
  },
  {
    key: 'cached',
    label: '缓存 Token',
    icon: Database,
    value: () => props.summary.cachedTokens,
    detail: () => '缓存读取命中',
    tone: 'text-(--cp-warning-text) bg-(--cp-warning-bg)',
  },
  {
    key: 'latency',
    label: '平均耗时',
    icon: Timer,
    value: () => props.summary.averageLatencyMs,
    detail: () => '成功请求平均值',
    tone: 'text-(--cp-normal-text) bg-(--cp-normal-bg)',
  },
]
</script>

<template>
  <section
    class="mt-5 grid shrink-0 grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-4"
    aria-label="使用概览"
  >
    <article
      v-for="item in items"
      :key="item.key"
      class="grid min-h-23 grid-cols-[36px_minmax(0,1fr)] items-stretch gap-3 rounded-(--cp-card-radius) bg-(--cp-bg-surface) px-4 py-3 shadow-(--cp-shadow-card)"
    >
      <span
        class="inline-flex size-9 shrink-0 items-center justify-center rounded-(--cp-input-radius-base)"
        :class="item.tone"
      >
        <component :is="item.icon" class="size-4.5" />
      </span>
      <div class="flex min-w-0 flex-col justify-between py-0.5">
        <span class="block text-[12px] leading-none font-bold text-(--cp-text-muted)">
          {{ item.label }}
        </span>
        <strong
          class="block truncate text-[22px] leading-none font-extrabold text-(--cp-text-primary)"
        >
          {{ item.value() }}
        </strong>
        <span class="block truncate text-[12px] leading-none font-[650] text-(--cp-text-secondary)">
          {{ item.detail() }}
        </span>
      </div>
    </article>
  </section>
</template>
