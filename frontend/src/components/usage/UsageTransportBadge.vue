<script setup lang="ts">
import { usageTransportType, usageTransportTypeClass } from './shared/presenter'

defineProps<{
  transport?: string | null
  joined?: boolean
}>()
</script>

<template>
  <span
    class="inline-flex h-6 min-w-10 items-center justify-center px-2 font-mono text-z-sm leading-none font-bold"
    :class="[usageTransportTypeClass(transport), { 'rounded-full': !joined }]"
  >
    {{ usageTransportType(transport) }}
  </span>
</template>
