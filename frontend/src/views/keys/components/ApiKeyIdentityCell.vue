<script setup lang="ts">
import type { ApiKey } from '@/api'

defineProps<{
  apiKey: ApiKey
}>()
</script>

<template>
  <div class="grid min-w-0 gap-1">
    <span class="truncate text-cp text-cp-text" :title="apiKey.name">
      {{ apiKey.name }}
    </span>
    <span v-if="apiKey.label" class="truncate text-cp-xs font-emphasis text-cp-text-quaternary" :title="apiKey.label">
      {{ apiKey.label }}
    </span>
  </div>
</template>
