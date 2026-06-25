<script setup lang="ts">
import { computed } from 'vue'

const props = withDefaults(
  defineProps<{
    label?: string
    title?: string
    size?: 'default' | 'sm' | 'md'
    variant?: 'default' | 'ghost'
    active?: boolean
    disabled?: boolean
    loading?: boolean
  }>(),
  {
    size: 'default',
    variant: 'default',
    active: false,
    disabled: false,
    loading: false,
  },
)

const sizeClasses = computed(() => {
  if (props.size === 'md') return 'h-11 w-11 rounded-xl'
  if (props.size === 'sm') return 'h-8 w-8 rounded-(--cp-icon-button-radius)'
  return 'h-9 w-9 rounded-xl'
})
</script>

<template>
  <button
    class="inline-flex items-center justify-center border-0 transition-colors outline-none focus-visible:ring-2 focus-visible:ring-(--cp-info-border) focus-visible:ring-offset-2 focus-visible:ring-offset-(--cp-bg-surface)"
    :class="[
      sizeClasses,
      variant === 'ghost'
        ? 'bg-transparent text-(--cp-text-secondary) hover:bg-(--cp-bg-subtle) shadow-none'
        : 'bg-(--cp-bg-surface) text-(--cp-text-secondary) shadow-(--cp-shadow-control) hover:bg-(--cp-default-bg-hover) hover:text-(--cp-normal)',
      active ? 'bg-(--cp-default-bg-hover) text-(--cp-normal)' : '',
      disabled || loading ? 'cursor-not-allowed opacity-50' : '',
    ]"
    :aria-label="label || title"
    :title="title || label"
    :aria-busy="loading"
    :disabled="disabled || loading"
    type="button"
  >
    <span class="inline-flex items-center justify-center" :class="loading ? 'animate-spin' : ''">
      <slot />
    </span>
  </button>
</template>
