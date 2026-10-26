<script setup lang="ts">
import type { getUsageRecordSummary } from '@/api'

import { ZCard, ZMotionIcon } from '@codex-proxy/ui'
import { Activity, Database, FileText, Timer } from '@lucide/vue'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  summary: Omit<Awaited<ReturnType<typeof getUsageRecordSummary>>, 'analytics'>
}>()

const { t } = useI18n({ useScope: 'global' })

const averageLatency = computed(() => {
  const raw = props.summary.averageLatencyMsValue
  const value = raw == null || !Number.isFinite(raw) || raw < 0
    ? 0
    : raw
  if (value < 1_000) {
    return {
      value: String(Math.round(value)),
      unit: 'ms',
    }
  }
  if (value < 60_000) {
    return {
      value: (value / 1_000).toFixed(2),
      unit: 's',
    }
  }
  if (value < 3_600_000) {
    return {
      value: (value / 60_000).toFixed(1),
      unit: 'min',
    }
  }
  return {
    value: (value / 3_600_000).toFixed(1),
    unit: 'h',
  }
})

const items = computed(() => [
  {
    key: 'requests',
    label: t('usage.successfulRequest'),
    icon: Activity,
    value: props.summary.totalRequests,
    detail: t('usage.withinFilterRange'),
    tone: 'bg-cp-blue-container text-cp-blue-on-container',
  },
  {
    key: 'tokens',
    label: t('usage.tokens.totalTokens'),
    icon: FileText,
    value: props.summary.totalTokens,
    detail: t('usage.inputOutput', { inputTokens: props.summary.inputTokens, outputTokens: props.summary.outputTokens }),
    tone: 'bg-cp-green-container text-cp-green-on-container',
  },
  {
    key: 'cached',
    label: t('usage.cachedTokens'),
    icon: Database,
    value: props.summary.cachedInputTokens,
    detail: t('usage.cacheReadHit'),
    tone: 'bg-cp-orange-container text-cp-orange-on-container',
  },
  {
    key: 'latency',
    label: t('usage.averageTimeTaken'),
    icon: Timer,
    value: averageLatency.value.value,
    unit: averageLatency.value.unit,
    detail: t('usage.successfulRequestAverage'),
    tone: 'bg-cp-cyan-container text-cp-cyan-on-container',
  },
])
</script>

<template>
  <section class="mt-5 grid shrink-0 grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-4" :aria-label="t('usage.usageOverview')">
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
      <div class="flex min-w-0 flex-col">
        <span class="block text-cp-sm leading-none font-heavy text-cp-text-secondary">
          {{ item.label }}
        </span>
        <strong class="my-2 block truncate font-mono text-[24px] leading-none font-extrabold text-cp-text">
          {{ item.value }}<span v-if="item.unit" class="ml-0.5 font-sans text-cp font-semibold text-cp-text-secondary">{{ item.unit }}</span>
        </strong>
        <span class="block truncate text-cp-sm leading-none font-emphasis text-cp-text-quaternary">
          {{ item.detail }}
        </span>
      </div>
    </ZCard>
  </section>
</template>
