<script setup lang="ts">
import { computed } from 'vue'

type BadgeVariant = 'normal' | 'success' | 'warning' | 'danger' | 'info' | 'default'

const props = withDefaults(
  defineProps<{
    variant?: BadgeVariant
    dot?: boolean
  }>(),
  {
    variant: 'default',
    dot: false,
  },
)

const variantClasses: Record<BadgeVariant, string> = {
  default: 'bg-(--cp-default-bg) text-(--cp-text-secondary)',
  normal: 'bg-(--cp-normal-bg) text-(--cp-normal-text)',
  success: 'bg-(--cp-success-bg) text-(--cp-success-text)',
  warning: 'bg-(--cp-warning-bg) text-(--cp-warning-text)',
  danger: 'bg-(--cp-danger-bg) text-(--cp-danger-text)',
  info: 'bg-(--cp-info-bg) text-(--cp-info-text)',
}

const badgeClasses = computed(() => [
  'inline-flex min-h-[26px] items-center justify-center gap-[7px] whitespace-nowrap rounded-(--cp-tag-radius) px-2.5 text-xs font-semibold leading-none',
  variantClasses[props.variant],
])
</script>

<template>
  <span :class="badgeClasses">
    <span v-if="dot" class="h-1.5 w-1.5 rounded-full bg-current" />
    <slot />
  </span>
</template>
