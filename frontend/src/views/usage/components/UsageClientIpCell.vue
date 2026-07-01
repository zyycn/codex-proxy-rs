<script setup lang="ts">
import { computed } from 'vue'

import { usageClientIp } from '../constants'

const props = defineProps<{
  record: any
}>()

const clientIp = computed(() => usageClientIp(props.record))
</script>

<template>
  <span
    class="inline-flex h-6 max-w-full items-center rounded-full bg-(--cp-info-bg) px-2.5 font-mono text-[12px] leading-none font-bold text-(--cp-info-text)"
    :title="clientIp"
  >
    <span class="min-w-0 truncate">
      {{ clientIp }}
    </span>
  </span>
</template>
