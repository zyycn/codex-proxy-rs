<script setup lang="ts">
import type { UsageModelRecord } from './shared/presenter'

import { CornerDownRight } from '@lucide/vue'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { usageModelDisplay } from './shared/presenter'

const props = defineProps<{
  record: UsageModelRecord
}>()

const modelDisplay = computed(() => usageModelDisplay(props.record))
const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <div class="inline-grid max-w-full gap-1">
    <code
      class="block max-w-full truncate font-mono text-z-sm leading-none font-heavy text-z-text"
      :title="t('usage.status.requestedModel', { model: modelDisplay.primary })"
    >
      {{ modelDisplay.primary }}
    </code>
    <div
      v-if="modelDisplay.secondary"
      class="flex min-w-0 max-w-full items-center gap-1.25 text-z-text-secondary"
      :title="t('usage.status.mappedModel', { secondary: modelDisplay.secondary })"
    >
      <CornerDownRight
        class="size-3.25 shrink-0 text-z-blue-text"
        stroke-width="2.4"
        aria-hidden="true"
      />
      <code class="block truncate font-mono text-z-xs leading-none font-bold">
        {{ modelDisplay.secondary }}
      </code>
    </div>
  </div>
</template>
