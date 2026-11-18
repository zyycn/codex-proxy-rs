<script setup lang="ts">
import type { UsageReasoningRecord } from './shared/presenter'

import { Bot, SquareDashedBottomCode } from '@lucide/vue'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { usageReasoningEffort } from './shared/presenter'

const props = defineProps<{
  record: UsageReasoningRecord
  inline?: boolean
}>()

const effort = computed(() => usageReasoningEffort(props.record))
const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <span
    class="inline-flex items-center gap-1 whitespace-nowrap text-z-sm font-bold text-z-text"
  >
    <span v-if="inline && effort !== '—'" class="font-mono font-medium text-z-text-secondary">({{ effort }})</span>
    <span v-else-if="!inline">{{ effort }}</span>
    <span
      v-if="record.subagentKind"
      class="inline-flex shrink-0 text-z-purple-text"
      :title="t('usage.reasoning.subagentRequest')"
      :aria-label="t('usage.reasoning.subagentRequest')"
    >
      <Bot class="size-3.5" stroke-width="2.2" />
    </span>
    <span
      v-if="record.subagentKind === 'review'"
      class="inline-flex shrink-0 text-z-purple-text"
      :title="t('usage.reasoning.reviewSubagentRequest')"
      :aria-label="t('usage.reasoning.reviewSubagentRequest')"
    >
      <SquareDashedBottomCode class="size-3.25" stroke-width="2.2" />
    </span>
  </span>
</template>
