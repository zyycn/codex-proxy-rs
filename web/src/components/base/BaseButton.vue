<script setup lang="ts">
import { LoaderCircle } from '@lucide/vue'
import { computed } from 'vue'

type ButtonVariant = 'default' | 'primary' | 'success' | 'warning' | 'danger' | 'plain' | 'text' | 'ghost'
type ButtonSize = 'small' | 'default' | 'large' | 'md'

const props = withDefaults(defineProps<{
  variant?: ButtonVariant
  size?: ButtonSize
  loading?: boolean
  disabled?: boolean
  type?: 'button' | 'submit' | 'reset'
}>(), {
  variant: 'primary',
  size: 'default',
  loading: false,
  disabled: false,
  type: 'button',
})

const sizeClasses = {
  large: 'h-10 px-4 gap-2 text-[12px] rounded-[6px]',
  default: 'h-8 px-3 gap-2 text-[12px] rounded-[6px]',
  md: 'h-8 px-3 gap-2 text-[12px] rounded-[6px]',
  small: 'h-6 px-2 gap-1.5 text-[11px] rounded-[4px]',
}

const variantClasses = {
  default: 'bg-[#F8FAFC] text-[#0E1726] hover:bg-[#F1F5F9]',
  primary: 'bg-[#2563EB] text-white hover:bg-[#1D4ED8]',
  success: 'bg-[#ECFDF5] text-[#047857] hover:bg-[#DDFBEA]',
  warning: 'bg-[#FFFBEB] text-[#B45309] hover:bg-[#FEF3C7]',
  danger: 'bg-[#FEF2F2] text-[#B91C1C] hover:bg-[#FEE2E2]',
  plain: 'bg-[#EEF6FF] text-[#2563EB] hover:bg-[#DBEAFE]',
  text: 'bg-transparent text-[#2563EB] hover:bg-transparent p-0 h-auto',
  ghost: 'bg-transparent text-[#64748B] hover:bg-[#F8FAFC]',
}

const classes = computed(() => [
  'inline-flex items-center justify-center border-0 font-[720] leading-[1.15] transition-all cursor-pointer',
  'disabled:cursor-not-allowed disabled:bg-[#F1F5F9] disabled:text-[#94A3B8]',
  sizeClasses[props.size],
  variantClasses[props.variant],
])
</script>

<template>
  <button :type="type" :class="classes" :disabled="disabled || loading">
    <LoaderCircle v-if="loading" class="animate-spin" :size="size === 'small' ? 12 : 14" />
    <span v-if="$slots.icon && !loading" class="inline-flex shrink-0">
      <slot name="icon" />
    </span>
    <slot />
  </button>
</template>
