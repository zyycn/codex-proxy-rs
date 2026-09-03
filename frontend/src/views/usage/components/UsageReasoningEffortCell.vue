<script setup lang="ts">
import type { UsageDisplayRecord } from '../utils/records'

import { Bot, SquareDashedBottomCode } from '@lucide/vue'
import { usageIsReview, usageIsSubagent, usageReasoningEffort } from '../utils/records'

defineProps<{
  record: UsageDisplayRecord
}>()
</script>

<template>
  <span
    class="inline-flex items-center gap-1 whitespace-nowrap text-cp-sm font-bold text-cp-text"
  >
    <span>{{ usageReasoningEffort(record) }}</span>
    <span
      v-if="usageIsSubagent(record)"
      class="inline-flex shrink-0 text-cp-purple-text"
      title="子代理请求"
      aria-label="子代理请求"
    >
      <Bot class="size-3.5" stroke-width="2.2" />
    </span>
    <span
      v-if="usageIsReview(record)"
      class="inline-flex shrink-0 text-cp-purple-text"
      title="Review 子代理请求"
      aria-label="Review 子代理请求"
    >
      <SquareDashedBottomCode class="size-3.25" stroke-width="2.2" />
    </span>
  </span>
</template>
