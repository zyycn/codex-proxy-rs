<script setup lang="ts">
defineProps<{
  statusCode?: number | null
}>()

function statusClass(statusCode?: number | null) {
  if (statusCode === undefined || statusCode === null) {
    return 'bg-(--cp-bg-subtle) text-(--cp-text-secondary)'
  }

  if (statusCode >= 200 && statusCode < 300) {
    return 'bg-(--cp-success-bg) text-(--cp-success-text)'
  }

  if (statusCode >= 300 && statusCode < 400) {
    return 'bg-(--cp-warning-bg) text-(--cp-warning-text)'
  }

  return 'bg-(--cp-danger-bg) text-(--cp-danger-text)'
}
</script>

<template>
  <span
    class="inline-flex h-6 min-w-12 items-center justify-center rounded-full px-2 font-mono text-[12px] leading-none font-bold"
    :class="statusClass(statusCode)"
  >
    {{ statusCode ?? '—' }}
  </span>
</template>
