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
    'bg-cp-accent text-cp-accent-on shadow-cp-control hover:bg-cp-accent-hover active:bg-cp-accent-pressed',
  secondary:
    'bg-cp-muted text-cp-primary shadow-cp-control hover:bg-cp-default-active active:bg-cp-nav-active',
  ghost:
    'bg-transparent text-cp-secondary shadow-none hover:bg-cp-subtle hover:text-cp-primary active:bg-cp-muted',
  destructive:
    'bg-cp-danger-bg text-cp-danger-text shadow-none hover:bg-cp-danger-bg-hover active:bg-cp-danger-bg-active',
}

const sizeClasses: Record<ButtonSize, string> = {
  sm: 'h-cp-control-sm gap-1.5 px-3 text-xs',
  md: 'h-cp-control-md gap-2 px-4 text-[13px]',
  lg: 'h-cp-control-lg gap-2.5 px-5 text-[14px]',
}

const spinnerSizes: Record<ButtonSize, number> = {
  sm: 14,
  md: 15,
  lg: 17,
}

const classes = computed(() => [
  'inline-flex shrink-0 touch-manipulation items-center justify-center rounded-cp-control-sm border-0 font-bold leading-none outline-none transition-[background-color,box-shadow,color,opacity,transform] duration-150 motion-safe:active:translate-y-px motion-safe:active:scale-[0.985] motion-reduce:transition-none',
  'focus-visible:ring-2 focus-visible:ring-cp-accent-border focus-visible:ring-offset-2 focus-visible:ring-offset-cp-surface',
  'disabled:cursor-not-allowed disabled:transform-none disabled:bg-cp-disabled disabled:text-cp-disabled-text disabled:shadow-none',
  sizeClasses[props.size],
  variantClasses[props.variant],
])
</script>

<template>
  <button
    :type="type"
    :class="classes"
    :disabled="disabled || loading"
    :aria-busy="loading || undefined"
  >
    <span
      v-if="loading"
      class="inline-grid shrink-0 place-items-center leading-none"
      aria-hidden="true"
    >
      <slot name="loading">
        <LoaderCircle
          class="animate-spin motion-reduce:animate-none"
          :size="spinnerSizes[size]"
        />
      </slot>
    </span>
    <span v-else-if="$slots.icon" class="inline-grid shrink-0 place-items-center" aria-hidden="true">
      <slot name="icon" />
    </span>
    <span class="inline-flex min-w-0 items-center justify-center gap-2">
      <slot />
    </span>
  </button>
</template>
