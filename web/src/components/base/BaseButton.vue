<script setup lang="ts">
import { LoaderCircle } from '@lucide/vue'
import { computed } from 'vue'

type ButtonVariant = 'default' | 'primary' | 'success' | 'warning' | 'danger' | 'ghost'

const props = withDefaults(
  defineProps<{
    variant?: ButtonVariant
    loading?: boolean
    disabled?: boolean
    type?: 'button' | 'submit' | 'reset'
  }>(),
  {
    variant: 'primary',
    loading: false,
    disabled: false,
    type: 'button',
  },
)

const variantClasses = {
  default:
    'bg-(--cp-bg-subtle) text-(--cp-text-primary) hover:bg-(--cp-default-bg-hover) active:bg-(--cp-default-bg-active)',
  primary:
    'bg-(--cp-info) text-(--cp-info-on) hover:bg-(--cp-info-hover) active:bg-(--cp-info-pressed)',
  success:
    'bg-(--cp-success-bg) text-(--cp-success-text) hover:bg-(--cp-success-bg-hover) active:bg-(--cp-success-bg-active)',
  warning:
    'bg-(--cp-warning-bg) text-(--cp-warning-text) hover:bg-(--cp-warning-bg-hover) active:bg-(--cp-warning-bg-active)',
  danger:
    'bg-(--cp-danger-bg) text-(--cp-danger-text) hover:bg-(--cp-danger-bg-hover) active:bg-(--cp-danger-bg-active)',
  ghost:
    'bg-transparent text-(--cp-text-secondary) hover:bg-(--cp-bg-subtle) active:bg-(--cp-bg-muted)',
}

const classes = computed(() => [
  'inline-flex h-8 items-center justify-center gap-2 rounded-(--cp-button-radius-base) border-0 px-3 text-xs font-[720] leading-[1.15] transition-all cursor-pointer outline-none',
  'focus-visible:ring-2 focus-visible:ring-(--cp-info-border) focus-visible:ring-offset-2 focus-visible:ring-offset-(--cp-bg-surface)',
  'disabled:cursor-not-allowed disabled:bg-(--cp-disabled-bg) disabled:text-(--cp-disabled-text) disabled:shadow-none',
  variantClasses[props.variant],
])
</script>

<template>
  <button :type="type" :class="classes" :disabled="disabled || loading">
    <LoaderCircle v-if="loading" class="animate-spin" :size="14" />
    <span v-if="$slots.icon && !loading" class="inline-flex shrink-0">
      <slot name="icon" />
    </span>
    <slot />
  </button>
</template>
