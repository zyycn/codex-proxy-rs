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
  success: 'bg-(--cp-success-bg) text-(--cp-success)',
  warning: 'bg-(--cp-warning-bg) text-(--cp-warning)',
  danger: 'bg-(--cp-danger-bg) text-(--cp-danger)',
  info: 'bg-(--cp-info-bg) text-(--cp-info)',
}
</script>

<template>
  <div class="inline-flex min-h-16 min-w-80 items-center gap-3 rounded-[18px] bg-(--cp-bg-surface) px-4 shadow-(--cp-shadow-popover)">
    <span class="inline-flex h-9.5 w-9.5 items-center justify-center rounded-xl" :class="iconClasses[variant]">
      <component :is="icon" :size="18" />
    </span>
    <span class="grid gap-1.25">
      <strong class="text-[13px] font-[760] leading-none text-(--cp-text-primary)">{{ title }}</strong>
      <span v-if="message" class="text-xs font-semibold leading-none text-(--cp-text-secondary)">{{ message }}</span>
    </span>
  </div>
</template>
