<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import HelpPopover from '@/components/HelpPopover.vue'

const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <span class="inline-flex items-center gap-1.5">
    <HelpPopover :label="t('usage.outputThroughput')">
      <div class="grid gap-2 text-left font-normal whitespace-normal">
        <p>{{ t('usage.performance.upstreamRateHelp') }}</p>
        <p>{{ t('usage.performance.localRateHelp') }}</p>
      </div>
    </HelpPopover>
    {{ t('usage.outputThroughput') }}
  </span>
</template>
