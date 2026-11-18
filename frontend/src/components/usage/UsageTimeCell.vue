<script setup lang="ts">
import { computed } from 'vue'

const props = defineProps<{ value: string, datetime?: string }>()
const parts = computed(() => props.value?.split(' '))
</script>

<template>
  <time v-if="datetime && parts?.length === 2" :datetime="datetime" :title="value" class="grid gap-1 font-mono tabular-nums">
    <span class="text-z-sm font-bold text-z-text">{{ parts[1] }}</span>
    <span class="text-z-xs text-z-text-tertiary">{{ parts[0] }}</span>
  </time>
  <span v-else class="block truncate font-mono text-z-sm leading-none font-bold text-z-text-secondary" :title="value">
    {{ value || '—' }}
  </span>
</template>
