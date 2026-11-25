<script setup lang="ts">
import type { UsageListRecord } from '@/api'

import { computed } from 'vue'

import { useI18n } from 'vue-i18n'
import { usageLatencyDetails } from './presenter'
import UsageDetailPopover from './UsageDetailPopover.vue'

const props = defineProps<{
  record: Pick<UsageListRecord, 'latencyDetails' | 'firstTokenLatencyMs' | 'latencyMs'>
}>()

const { t } = useI18n({ useScope: 'global' })

const latencyDetails = computed(() => usageLatencyDetails(props.record))

const latencyTones = {
  good: 'text-z-success-text',
  warn: 'text-z-warning-text',
  // 主题的 Orange 与 Warning 共用色种，混入 Error 让相邻两档可区分
  slow: 'latency-slow',
  critical: 'text-z-error-text',
}
</script>

<template>
  <div class="flex items-center justify-end gap-1.5">
    <div
      class="grid grid-cols-[auto_auto] items-center justify-end gap-x-2 gap-y-1.5 whitespace-nowrap font-mono text-z-sm leading-none font-heavy tabular-nums"
    >
      <span class="text-z-xs text-z-text-quaternary">{{ t('usage.ttft') }}</span>
      <span :class="latencyDetails.firstOutputSeverity === null ? 'text-z-text-quaternary' : latencyTones[latencyDetails.firstOutputSeverity]">
        {{ latencyDetails.firstOutputDisplay }}
      </span>
      <span class="text-z-xs text-z-text-quaternary">{{ t('usage.latency.total') }}</span>
      <span :class="latencyDetails.totalSeverity === null ? 'text-z-text-quaternary' : latencyTones[latencyDetails.totalSeverity]">
        {{ latencyDetails.totalDisplay }}
      </span>
    </div>

    <UsageDetailPopover :trigger-label="t('usage.latency.viewLatencyDetails')">
      <section
        v-for="(section, index) in latencyDetails.sections"
        :key="section.title"
        class="grid gap-1.5 text-z-text-secondary"
        :class="{ 'border-t border-z-split pt-2': index > 0 }"
      >
        <p class="m-0 flex items-center justify-between gap-3 text-z-sm font-heavy text-z-text">
          <span>{{ section.title }}</span>
          <span
            class="rounded px-1 py-0.5 text-[10px] leading-none font-normal"
            :class="section.source === 'official'
              ? 'bg-z-primary-container text-z-primary-on-container'
              : 'bg-z-fill-tertiary text-z-text-tertiary'"
          >
            {{ section.source === 'official' ? t('usage.latency.official') : t('usage.latency.local') }}
          </span>
        </p>
        <div
          v-for="item in section.items"
          :key="item.label"
          class="flex justify-between gap-4 text-z-xs"
        >
          <span class="whitespace-nowrap">{{ item.label }}</span>
          <span
            class="whitespace-nowrap font-mono font-heavy"
            :class="item.emphasized ? 'text-z-info-text' : 'text-z-text'"
          >
            {{ item.value }}
          </span>
        </div>
      </section>
    </UsageDetailPopover>
  </div>
</template>

<style scoped>
.latency-slow {
  color: color-mix(in oklab, var(--z-color-warning-text) 65%, var(--z-color-error-text));
}
</style>
