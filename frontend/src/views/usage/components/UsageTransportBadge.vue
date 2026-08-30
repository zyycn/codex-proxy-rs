<script setup lang="ts">
import { usageTransportType, usageTransportTypeClass } from '../utils/records'

defineProps<{
  transport?: string | null
}>()
</script>

<template>
  <span
    class="inline-flex h-6 min-w-12 items-center justify-center rounded-full px-2 text-cp leading-none font-bold"
    :class="usageTransportTypeClass(transport)"
  >
    {{ usageTransportType(transport) }}
  </span>
</template>
