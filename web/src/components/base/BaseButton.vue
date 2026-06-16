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
  small: 'h-6 rounded-(--cp-button-radius-small) px-2.5 text-xs',
}

const variantClasses: Record<ButtonVariant, string> = {
  default: 'bg-(--cp-default-bg) text-(--cp-default-text) hover:bg-(--cp-default-bg-hover)',
  primary: 'bg-(--cp-info) text-(--cp-info-on) hover:bg-(--cp-info-hover)',
  success: 'bg-(--cp-success-bg) text-(--cp-success-text) hover:bg-(--cp-success-bg-hover)',
  warning: 'bg-(--cp-warning-bg) text-(--cp-warning-text) hover:bg-(--cp-warning-bg-hover)',
  danger: 'bg-(--cp-danger-bg) text-(--cp-danger-text) hover:bg-(--cp-danger-bg-hover)',
  plain: 'bg-(--cp-info-bg) text-(--cp-info-text) hover:bg-(--cp-info-bg-hover)',
  text: 'h-auto bg-transparent p-0 text-(--cp-info-text) hover:text-(--cp-info-hover)',
}

const buttonClasses = computed(() => [
  'inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-(--cp-button-radius-base) border-0 font-bold leading-none transition-colors disabled:cursor-not-allowed disabled:bg-(--cp-disabled-bg) disabled:text-(--cp-disabled-text)',
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
