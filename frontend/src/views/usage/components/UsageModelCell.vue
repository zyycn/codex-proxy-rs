<script setup lang="ts">
import type { UsageDisplayRecord } from '../utils/records'
import { CornerDownRight } from '@lucide/vue'

import { computed } from 'vue'
import { usageModelDisplay } from '../utils/records'

const props = defineProps<{
  record: UsageDisplayRecord
}>()

const modelDisplay = computed(() => usageModelDisplay(props.record))
</script>

<template>
  <div class="inline-grid max-w-full gap-1">
    <code
      class="block max-w-full truncate font-mono text-cp-sm leading-none font-heavy text-cp-text"
      :title="`请求模型：${modelDisplay.primary}`"
    >
      {{ modelDisplay.primary }}
    </code>
    <div
      v-for="route in modelDisplay.routes"
      :key="route.kind"
      class="flex min-w-0 max-w-full items-center gap-1.25 text-cp-text-secondary"
      :title="route.description"
    >
      <CornerDownRight
        class="size-3.25 shrink-0"
        :class="route.kind === 'returned' ? 'text-cp-orange-text' : 'text-cp-blue-text'"
        stroke-width="2.4"
        aria-hidden="true"
      />
      <code class="block truncate font-mono text-cp-xs leading-none font-bold">
        {{ route.model }}
      </code>
    </div>
  </div>
</template>
