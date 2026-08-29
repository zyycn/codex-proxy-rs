<script setup lang="ts">
import type { UsageDisplayRecord } from '../utils/records'

import { computed } from 'vue'
import { usageClientIp } from '../utils/records'

const props = defineProps<{
  record: UsageDisplayRecord
}>()

const clientIp = computed(() => usageClientIp(props.record))
</script>

<template>
  <span
    class="inline-flex h-6 max-w-full items-center rounded-full bg-cp-blue-bg px-2.5 font-mono text-cp-sm leading-none font-bold text-cp-blue-text-on-bg"
    :title="clientIp"
  >
    <span class="min-w-0 truncate">
      {{ clientIp }}
    </span>
  </span>
</template>
