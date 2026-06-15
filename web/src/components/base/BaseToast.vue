<script setup lang="ts">
import { AlertCircle, CheckCircle2, Info, TriangleAlert } from '@lucide/vue'
import { computed } from 'vue'

type ToastVariant = 'success' | 'warning' | 'danger' | 'info'

const props = withDefaults(defineProps<{
  variant?: ToastVariant
  title: string
  message?: string
}>(), {
  variant: 'info',
  message: undefined,
})

const icon = computed(() => {
  switch (props.variant) {
    case 'success':
      return CheckCircle2
    case 'warning':
      return TriangleAlert
    case 'danger':
      return AlertCircle
    default:
      return Info
  }
})

const iconClasses: Record<ToastVariant, string> = {
  success: 'bg-[var(--cp-success-bg)] text-[var(--cp-success)]',
  warning: 'bg-[var(--cp-warning-bg)] text-[var(--cp-warning)]',
  danger: 'bg-[var(--cp-danger-bg)] text-[var(--cp-danger)]',
  info: 'bg-[var(--cp-info-bg)] text-[var(--cp-info)]',
}
</script>

<template>
  <div class="inline-flex min-h-16 min-w-80 items-center gap-3 rounded-[18px] bg-[var(--cp-bg-surface)] px-4 shadow-[var(--cp-shadow-popover)]">
    <span class="inline-flex h-[38px] w-[38px] items-center justify-center rounded-xl" :class="iconClasses[variant]">
      <component :is="icon" :size="18" />
    </span>
    <span class="grid gap-[5px]">
      <strong class="text-[13px] font-[760] leading-none text-[var(--cp-text-primary)]">{{ title }}</strong>
      <span v-if="message" class="text-xs font-semibold leading-none text-[var(--cp-text-secondary)]">{{ message }}</span>
    </span>
  </div>
</template>
