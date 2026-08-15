<script setup lang="ts">
import { LoaderCircle } from '@lucide/vue'
import { computed } from 'vue'

type IconButtonVariant = 'primary' | 'secondary' | 'success' | 'ghost' | 'destructive'
type IconButtonSize = 'sm' | 'md' | 'lg'

const props = withDefaults(
  defineProps<{
    label: string
    variant?: IconButtonVariant
    size?: IconButtonSize
    loading?: boolean
    disabled?: boolean
    pressed?: boolean
    type?: 'button' | 'submit' | 'reset'
  }>(),
  {
    variant: 'ghost',
    size: 'md',
    loading: false,
    disabled: false,
    pressed: false,
    type: 'button',
  },
)

defineSlots<{
  loading?: () => unknown
  default: () => unknown
}>()

const variantClasses: Record<IconButtonVariant, string> = {
  primary:
    'bg-cp-accent text-cp-accent-on shadow-cp-control hover:bg-cp-accent-hover active:bg-cp-accent-pressed',
  secondary:
    'bg-cp-surface text-cp-secondary shadow-cp-control hover:bg-cp-default-hover hover:text-cp-primary active:bg-cp-default-active',
  success:
    'bg-cp-success-bg text-cp-success-text shadow-none hover:bg-cp-success-bg-hover active:bg-cp-success-bg-active',
  ghost:
    'bg-transparent text-cp-secondary shadow-none hover:bg-cp-subtle hover:text-cp-primary active:bg-cp-muted',
  destructive:
    'bg-transparent text-cp-danger-text shadow-none hover:bg-cp-danger-bg-hover active:bg-cp-danger-bg-active',
}

const sizeClasses: Record<IconButtonSize, string> = {
  sm: 'size-cp-control-sm [&>svg]:size-3.5',
  md: 'size-cp-control-md [&>svg]:size-4',
  lg: 'size-cp-control-lg [&>svg]:size-4.5',
}

const spinnerSizes: Record<IconButtonSize, number> = {
  sm: 14,
  md: 16,
  lg: 18,
}

const classes = computed(() => [
  'inline-grid shrink-0 touch-manipulation place-items-center rounded-cp-control border-0 leading-none outline-none transition-[background-color,box-shadow,color,opacity,transform] duration-150 motion-safe:active:scale-[0.96] motion-reduce:transition-none',
  'focus-visible:ring-2 focus-visible:ring-cp-accent-border focus-visible:ring-offset-2 focus-visible:ring-offset-cp-surface',
  'disabled:cursor-not-allowed disabled:transform-none disabled:bg-cp-disabled disabled:text-cp-disabled-text disabled:shadow-none',
  sizeClasses[props.size],
  variantClasses[props.variant],
  props.pressed ? 'bg-cp-default-active text-cp-primary' : undefined,
])
</script>

<template>
  <button
    :type="type"
    :class="classes"
    :disabled="disabled || loading"
    :aria-label="label"
    :aria-pressed="pressed || undefined"
    :aria-busy="loading || undefined"
    :title="label"
  >
    <span v-if="loading" class="inline-grid place-items-center" aria-hidden="true">
      <slot name="loading">
        <LoaderCircle
          class="animate-spin motion-reduce:animate-none"
          :size="spinnerSizes[size]"
        />
      </slot>
    </span>
    <span v-else class="inline-grid place-items-center" aria-hidden="true">
      <slot />
    </span>
  </button>
</template>
