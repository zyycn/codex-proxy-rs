<script setup lang="ts">
import { computed } from 'vue'

type BadgeVariant = 'normal' | 'success' | 'warning' | 'danger' | 'info' | 'default'

const props = withDefaults(defineProps<{
  variant?: BadgeVariant
  dot?: boolean
}>(), {
  variant: 'default',
  dot: false,
})

const variantClasses: Record<BadgeVariant, string> = {
  default: 'bg-[var(--cp-default-bg)] text-[var(--cp-text-secondary)]',
  normal: 'bg-[var(--cp-normal-bg)] text-[var(--cp-normal-text)]',
  success: 'bg-[var(--cp-success-bg)] text-[var(--cp-success-text)]',
  warning: 'bg-[var(--cp-warning-bg)] text-[var(--cp-warning-text)]',
  danger: 'bg-[var(--cp-danger-bg)] text-[var(--cp-danger-text)]',
  info: 'bg-[var(--cp-info-bg)] text-[var(--cp-info-text)]',
}

const badgeClasses = computed(() => [
  'inline-flex min-h-[26px] items-center justify-center gap-[7px] whitespace-nowrap rounded-[var(--cp-tag-radius)] px-2.5 text-xs font-semibold leading-none',
  variantClasses[props.variant],
])
</script>

<template>
  <span :class="badgeClasses">
    <span v-if="dot" class="h-1.5 w-1.5 rounded-full bg-current" />
    <slot />
  </span>
</template>
