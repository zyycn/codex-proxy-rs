<script setup lang="ts">
import { LoaderCircle } from '@lucide/vue'
import { computed, useSlots } from 'vue'

type ButtonVariant = 'default' | 'primary' | 'success' | 'warning' | 'danger' | 'ghost'
type ButtonSize = 'sm' | 'default' | 'md' | 'lg'

const props = withDefaults(
  defineProps<{
    variant?: ButtonVariant
    size?: ButtonSize
    loading?: boolean
    disabled?: boolean
    active?: boolean
    iconOnly?: boolean
    label?: string
    title?: string
    type?: 'button' | 'submit' | 'reset'
  }>(),
  {
    variant: 'primary',
    size: 'default',
    loading: false,
    disabled: false,
    active: false,
    iconOnly: false,
    type: 'button',
  },
)

const slots = useSlots()

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

const iconOnlyDefaultClasses =
  'bg-(--cp-bg-surface) text-(--cp-text-secondary) shadow-(--cp-shadow-control) hover:bg-(--cp-default-bg-hover) hover:text-(--cp-normal) active:bg-(--cp-default-bg-active)'

const sizeClasses: Record<ButtonSize, string> = {
  sm: 'h-7 gap-1.5 px-3 text-xs',
  default: 'h-9 gap-2 px-4 text-[13px]',
  md: 'h-11 gap-2 px-4 text-[13px]',
  lg: 'h-10.5 gap-2.5 px-5 text-[15px]',
}

const iconOnlySizeClasses: Record<ButtonSize, string> = {
  sm: 'size-8 rounded-(--cp-icon-button-radius)',
  default: 'size-9 rounded-(--cp-icon-button-radius)',
  md: 'size-11 rounded-(--cp-icon-button-radius)',
  lg: 'size-10.5 rounded-(--cp-icon-button-radius)',
}

const loadingIconSize: Record<ButtonSize, number> = {
  sm: 14,
  default: 15,
  md: 16,
  lg: 17,
}

const classes = computed(() => [
  'inline-flex items-center justify-center border-0 font-[720] leading-[1.15] transition-[background-color,box-shadow,color,opacity,transform] duration-150 cursor-pointer outline-none',
  props.iconOnly ? iconOnlySizeClasses[props.size] : sizeClasses[props.size],
  props.iconOnly ? '' : 'rounded-(--cp-button-radius-base)',
  'focus-visible:ring-2 focus-visible:ring-(--cp-info-border) focus-visible:ring-offset-2 focus-visible:ring-offset-(--cp-bg-surface)',
  'disabled:cursor-not-allowed disabled:bg-(--cp-disabled-bg) disabled:text-(--cp-disabled-text) disabled:shadow-none',
  props.iconOnly && (props.variant === 'default' || props.variant === 'primary')
    ? iconOnlyDefaultClasses
    : variantClasses[props.variant],
  props.active ? 'bg-(--cp-default-bg-hover) text-(--cp-normal)' : undefined,
])

const ariaLabel = computed(() => props.label || props.title)

const labelText = computed(() => {
  const defaultSlot = slots.default?.()
  const text = defaultSlot
    ?.map((node) => (typeof node.children === 'string' ? node.children : ''))
    .join('')
    .trim()

  return text || ''
})

const labelClasses = computed(() => [
  'inline-flex min-w-0 items-center justify-center',
  labelText.value.length === 2 ? 'tracking-[0.12em]' : undefined,
])
</script>

<template>
  <button
    :type="type"
    :class="classes"
    :disabled="disabled || loading"
    :aria-label="iconOnly ? ariaLabel : undefined"
    :title="title || (iconOnly ? label : undefined)"
    :aria-busy="loading"
  >
    <LoaderCircle v-if="loading" class="animate-spin" :size="loadingIconSize[size]" />
    <span v-if="$slots.icon && !loading" class="inline-flex shrink-0">
      <slot name="icon" />
    </span>
    <span v-if="!iconOnly && $slots.default" :class="labelClasses">
      <slot />
    </span>
    <span
      v-if="iconOnly && !$slots.icon && !loading"
      class="inline-flex items-center justify-center"
    >
      <slot />
    </span>
  </button>
</template>
