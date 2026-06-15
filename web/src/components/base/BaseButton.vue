<script setup lang="ts">
import { LoaderCircle } from '@lucide/vue'
import { computed } from 'vue'

type ButtonVariant = 'default' | 'primary' | 'success' | 'warning' | 'danger' | 'plain' | 'text'
type ButtonSize = 'small' | 'default' | 'large'

const props = withDefaults(defineProps<{
  variant?: ButtonVariant
  size?: ButtonSize
  loading?: boolean
  disabled?: boolean
  type?: 'button' | 'submit' | 'reset'
}>(), {
  variant: 'default',
  size: 'default',
  loading: false,
  disabled: false,
  type: 'button',
})

const sizeClasses: Record<ButtonSize, string> = {
  large: 'h-10 px-4 text-[13px]',
  default: 'h-8 px-3.5 text-[13px]',
  small: 'h-6 rounded-[var(--cp-button-radius-small)] px-2.5 text-xs',
}

const variantClasses: Record<ButtonVariant, string> = {
  default: 'bg-[var(--cp-default-bg)] text-[var(--cp-default-text)] hover:bg-[var(--cp-default-bg-hover)]',
  primary: 'bg-[var(--cp-info)] text-[var(--cp-info-on)] hover:bg-[var(--cp-info-hover)]',
  success: 'bg-[var(--cp-success-bg)] text-[var(--cp-success-text)] hover:bg-[var(--cp-success-bg-hover)]',
  warning: 'bg-[var(--cp-warning-bg)] text-[var(--cp-warning-text)] hover:bg-[var(--cp-warning-bg-hover)]',
  danger: 'bg-[var(--cp-danger-bg)] text-[var(--cp-danger-text)] hover:bg-[var(--cp-danger-bg-hover)]',
  plain: 'bg-[var(--cp-info-bg)] text-[var(--cp-info-text)] hover:bg-[var(--cp-info-bg-hover)]',
  text: 'h-auto bg-transparent p-0 text-[var(--cp-info-text)] hover:text-[var(--cp-info-hover)]',
}

const buttonClasses = computed(() => [
  'inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-[var(--cp-button-radius-base)] border-0 font-bold leading-none transition-colors disabled:cursor-not-allowed disabled:bg-[var(--cp-disabled-bg)] disabled:text-[var(--cp-disabled-text)]',
  sizeClasses[props.size],
  variantClasses[props.variant],
])
</script>

<template>
  <button :type="type" :class="buttonClasses" :disabled="disabled || loading">
    <LoaderCircle v-if="loading" class="animate-spin" :size="14" />
    <span v-if="$slots.icon && !loading" class="inline-flex shrink-0">
      <slot name="icon" />
    </span>
    <span>
      <slot />
    </span>
  </button>
</template>
