<script setup lang="ts">
import type { ApiKey } from '@/api'

defineProps<{
  apiKey: ApiKey
}>()
</script>

<template>
  <div class="grid min-w-0 gap-1">
    <strong class="truncate text-z text-z-text" :title="apiKey.name">
      {{ apiKey.name }}
    </strong>
    <span v-if="apiKey.label" class="truncate text-z-xs font-emphasis text-z-text-quaternary" :title="apiKey.label">
      {{ apiKey.label }}
    </span>
  </div>
</template>
