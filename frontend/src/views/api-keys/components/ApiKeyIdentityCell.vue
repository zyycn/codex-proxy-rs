<script setup lang="ts">
import type { getApiKeys } from '@/api'

type ApiKeyRow = Awaited<ReturnType<typeof getApiKeys>>['items'][number]

defineProps<{
  apiKey: ApiKeyRow
}>()
</script>

<template>
  <div class="flex flex-col gap-0.5">
    <span class="text-[13px] font-bold text-(--cp-text-primary)">
      {{ apiKey.name }}
    </span>
    <span v-if="apiKey.label" class="text-[12px] font-[650] text-(--cp-text-tertiary)">
      {{ apiKey.label }}
    </span>
  </div>
</template>
