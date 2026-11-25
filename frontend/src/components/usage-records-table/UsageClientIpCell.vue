<script setup lang="ts">
import { computed } from 'vue'
import { usageClientIp } from './presenter'

const props = defineProps<{
  record: { clientIp?: string | null }
}>()

const clientIp = computed(() => usageClientIp(props.record))
</script>

<template>
  <span
    class="inline-flex h-6 max-w-full items-center rounded-full bg-z-blue-container px-2.5 font-mono text-z-sm leading-none font-bold text-z-blue-on-container"
    :title="clientIp"
  >
    <span class="min-w-0 truncate">
      {{ clientIp }}
    </span>
  </span>
</template>
