<script setup lang="ts">
defineProps<{
  apiKey: {
    enabled: boolean
  }
}>()
</script>

<template>
  <span
    class="inline-flex h-6 min-w-14 items-center justify-center rounded-full px-2 text-[12px] leading-none font-bold"
    :class="{
      'bg-(--cp-success-bg) text-(--cp-success-text)': apiKey.enabled,
      'bg-(--cp-bg-subtle) text-(--cp-text-secondary)': !apiKey.enabled,
    }"
  >
    {{ apiKey.enabled ? '已启用' : '已禁用' }}
  </span>
</template>
