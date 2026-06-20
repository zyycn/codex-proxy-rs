<script setup lang="ts">
withDefaults(defineProps<{
  label?: string
  title?: string
  size?: 'default' | 'large' | 'sm' | 'md'
  variant?: 'default' | 'ghost'
  active?: boolean
  disabled?: boolean
}>(), {
  size: 'default',
  variant: 'default',
  active: false,
  disabled: false,
})
</script>

<template>
  <button
    class="inline-flex items-center justify-center border-0 shadow-(--cp-shadow-control) transition-colors"
    :class="[
      size === 'large' || size === 'md' ? 'h-11 w-11 rounded-xl' : size === 'sm' ? 'h-8 w-8 rounded-lg' : 'h-9 w-9 rounded-xl',
      variant === 'ghost' ? 'bg-transparent text-(--cp-text-secondary) hover:bg-(--cp-bg-tertiary) shadow-none' : 'bg-(--cp-bg-surface) text-(--cp-text-secondary) hover:bg-(--cp-default-bg-hover) hover:text-(--cp-normal)',
      active ? 'bg-(--cp-default-bg-hover) text-(--cp-normal)' : '',
      disabled ? 'cursor-not-allowed opacity-50' : '',
    ]"
    :aria-label="label || title"
    :title="title || label"
    :disabled="disabled"
    type="button"
  >
    <slot />
  </button>
</template>
