<script setup lang="ts">
import { LoaderCircle } from '@lucide/vue'
import { computed } from 'vue'

type ButtonVariant = 'primary' | 'secondary' | 'ghost' | 'destructive'
type ButtonSize = 'sm' | 'md' | 'lg'

const props = withDefaults(
  defineProps<{
    variant?: ButtonVariant
    size?: ButtonSize
    loading?: boolean
    disabled?: boolean
    type?: 'button' | 'submit' | 'reset'
  }>(),
  {
    variant: 'secondary',
    size: 'md',
    loading: false,
    disabled: false,
    type: 'button',
  },
)

defineSlots<{
  icon?: () => unknown
  loading?: () => unknown
  default: () => unknown
}>()

const variantClasses: Record<ButtonVariant, string> = {
  primary:
    'bg-(--cp-button-primary-bg) text-(--cp-button-primary-color) shadow-cp-tertiary hover:bg-(--cp-button-primary-hover-bg) active:bg-(--cp-button-primary-active-bg)',
  secondary:
    'bg-cp-fill-tertiary text-cp-text shadow-cp-tertiary hover:bg-cp-bg-text-active active:bg-cp-bg-text-active',
  ghost:
    'bg-transparent text-cp-text-secondary shadow-none hover:bg-cp-fill-quaternary hover:text-cp-text active:bg-cp-fill-tertiary',
  destructive: 'bg-cp-error-bg text-cp-error-text shadow-none hover:bg-cp-error-bg-hover active:bg-cp-error-bg-active',
}

const sizeClasses: Record<ButtonSize, string> = {
  sm: 'h-cp-control-sm gap-1.5 px-3 text-xs',
  md: 'h-cp-control gap-2 px-4 text-cp',
  lg: 'h-cp-control-lg gap-2.5 px-5 text-cp-lg',
}

const spinnerSizes: Record<ButtonSize, number> = {
  sm: 14,
  md: 15,
  lg: 17,
}

const classes = computed(() => [
  'inline-flex shrink-0 touch-manipulation items-center justify-center rounded-cp-sm border-0 font-bold leading-none outline-none transition-[background-color,box-shadow,color,opacity,transform] duration-150 motion-safe:active:translate-y-px motion-safe:active:scale-[0.985] motion-reduce:transition-none',
  'focus-visible:ring-2 focus-visible:ring-cp-control-outline focus-visible:ring-offset-2 focus-visible:ring-offset-cp-bg-container',
  'disabled:cursor-not-allowed disabled:transform-none disabled:bg-cp-bg-container-disabled disabled:text-cp-text-disabled disabled:shadow-none',
  sizeClasses[props.size],
  variantClasses[props.variant],
])
</script>

<template>
  <button :type="type" :class="classes" :disabled="disabled || loading" :aria-busy="loading || undefined">
    <span v-if="loading" class="inline-grid shrink-0 place-items-center leading-none">
      <slot name="loading">
        <LoaderCircle class="animate-spin motion-reduce:animate-none" :size="spinnerSizes[size]" />
      </slot>
    </span>
    <span v-else-if="$slots.icon" class="inline-grid shrink-0 place-items-center">
      <slot name="icon" />
    </span>
    <span class="inline-flex min-w-0 items-center justify-center gap-2">
      <slot />
    </span>
  </button>
</template>
