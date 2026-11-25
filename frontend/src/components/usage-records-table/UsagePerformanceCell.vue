<script setup lang="ts">
import type { UsagePerformanceRecord } from './presenter'

import { computed } from 'vue'

import { useI18n } from 'vue-i18n'
import { usagePerformanceDetails } from './presenter'

const props = defineProps<{
  record: UsagePerformanceRecord
}>()

const { t } = useI18n({ useScope: 'global' })

const performance = computed(() => usagePerformanceDetails(props.record))
</script>

<template>
  <div class="grid justify-items-end gap-y-1.5 whitespace-nowrap font-mono text-z-sm leading-none font-heavy tabular-nums">
    <span class="text-z-text">{{ performance.throughputDisplay }}</span>
    <span class="text-z-xs text-z-text-secondary">
      <span :title="t('usage.performance.ttftHelp')" class="text-z-text-quaternary">TTFT</span>
      {{ performance.firstTokenDisplay }}
    </span>
  </div>
</template>
