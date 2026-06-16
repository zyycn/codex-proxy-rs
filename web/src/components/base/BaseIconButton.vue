<script setup lang="ts">
withDefaults(defineProps<{
  label: string
  size?: 'default' | 'large'
  active?: boolean
}>(), {
  size: 'default',
  active: false,
})
</script>

<template>
  <button
    class="inline-flex items-center justify-center border-0 bg-(--cp-bg-surface) text-(--cp-text-secondary) shadow-(--cp-shadow-control) transition-colors hover:bg-(--cp-default-bg-hover) hover:text-(--cp-normal)"
    :class="[
      size === 'large' ? 'h-11 w-11 rounded-xl' : 'h-9 w-9 rounded-xl',
      active ? 'bg-(--cp-default-bg-hover) text-(--cp-normal)' : '',
    ]"
    :aria-label="label"
    :title="label"
    type="button"
  >
    <slot />
  </button>
</template>
