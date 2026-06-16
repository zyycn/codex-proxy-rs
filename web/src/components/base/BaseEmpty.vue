<script setup lang="ts">
defineProps<{
  title?: string
  description?: string
}>()
</script>

<template>
  <div class="grid justify-items-center gap-2 rounded-[14px] bg-(--cp-bg-subtle) p-5 text-center">
    <div class="h-1 w-7 rounded-full bg-(--cp-default-border-hover)" />
    <p class="m-0 text-[13px] font-bold text-(--cp-text-primary)">{{ title ?? '暂无数据' }}</p>
    <p v-if="description" class="m-0 text-xs font-semibold text-(--cp-text-secondary)">{{ description }}</p>
  </div>
</template>
