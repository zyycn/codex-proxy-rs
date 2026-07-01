<script setup lang="ts">
import { CornerDownRight } from '@lucide/vue'
import { computed } from 'vue'

import { usageModelDisplay } from '../constants'

const props = defineProps<{
  record: any
}>()

const modelDisplay = computed(() => usageModelDisplay(props.record))
</script>

<template>
  <div class="inline-grid max-w-full gap-1">
    <code
      class="block max-w-full truncate font-mono text-[12px] leading-none font-[760] text-(--cp-text-primary)"
    >
      {{ modelDisplay.primary }}
    </code>
    <div
      v-if="modelDisplay.secondary"
      class="flex min-w-0 items-center gap-1.25 text-(--cp-text-secondary)"
    >
      <CornerDownRight class="size-3.25 shrink-0 text-(--cp-info)" stroke-width="2.4" />
      <code class="block truncate font-mono text-[11px] leading-none font-bold">
        {{ modelDisplay.secondary }}
      </code>
    </div>
  </div>
</template>
