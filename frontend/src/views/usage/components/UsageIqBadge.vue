<script setup lang="ts">
import { usageIqClass, usageIqText } from '../utils/records'

defineProps<{
  bytes?: number | null
}>()
</script>

<template>
  <span
    class="inline-flex h-6 min-w-12 items-center justify-center rounded-full px-2 text-cp leading-none font-bold tabular-nums"
    :class="usageIqClass(bytes)"
  >
    {{ usageIqText(bytes) }}
  </span>
</template>
