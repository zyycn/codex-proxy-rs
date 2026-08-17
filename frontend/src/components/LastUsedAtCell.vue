<script setup lang="ts">
import { computed } from 'vue'

import { useUiClock } from '@/composables/useUiClock'
import { formatDateTime, formatRelativeTime } from '@/utils/date'

const props = defineProps<{
  value: string | null
}>()

const now = useUiClock()
const displayValue = computed(() => props.value ? formatRelativeTime(props.value, now.value) : '')
const fullValue = computed(() => props.value ? formatDateTime(props.value) : undefined)
</script>

<template>
  <span v-if="displayValue" :title="fullValue">
    {{ displayValue }}
  </span>
</template>
