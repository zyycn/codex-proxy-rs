<script setup lang="ts">
import { LoaderCircle } from '@lucide/vue'

withDefaults(defineProps<{ label?: string }>(), {
  label: '加载中',
})
</script>

<template>
  <span class="inline-flex items-center gap-2.5 text-[13px] font-semibold text-(--cp-text-secondary)" role="status">
    <LoaderCircle class="animate-spin text-(--cp-info)" :size="16" />
    <span>{{ label }}</span>
  </span>
</template>
