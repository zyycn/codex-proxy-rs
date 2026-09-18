<script setup lang="ts">
import { X } from '@lucide/vue'
import { computed, useId } from 'vue'

type TagType = 'neutral' | 'primary' | 'success' | 'info' | 'warning' | 'danger'
type TagSize = 'sm' | 'md' | 'lg'

const props = withDefaults(defineProps<{
  type?: TagType
  size?: TagSize
  round?: boolean
  closable?: boolean
  disabled?: boolean
  closeLabel?: string
}>(), {
  type: 'neutral',
  size: 'md',
  round: false,
  closable: false,
  disabled: false,
  closeLabel: undefined,
})

// 关闭只通知调用方，由调用方决定是否移除标签。
defineEmits<{ close: [event: MouseEvent] }>()
defineSlots<{ default: () => unknown }>()
const contentId = useId()
const closeId = useId()

const typeClasses: Record<TagType, string> = {
  neutral: 'bg-cp-fill-tertiary text-cp-text-secondary',
  primary: 'bg-cp-primary-container text-cp-primary-on-container',
  success: 'bg-cp-success-container text-cp-success-on-container',
  info: 'bg-cp-info-container text-cp-info-on-container',
  warning: 'bg-cp-warning-container text-cp-warning-on-container',
  danger: 'bg-cp-error-container text-cp-error-on-container',
}
const sizeClasses: Record<TagSize, string> = {
  sm: 'min-h-5 gap-1 px-1.5 py-0.5 text-cp-xs',
  md: 'min-h-6 gap-1.5 px-2 py-0.5 text-cp-xs',
  lg: 'min-h-7 gap-2 px-2.5 py-1 text-cp-sm',
}
const classes = computed(() => [
  sizeClasses[props.size],
  props.round ? 'rounded-full' : 'rounded-cp',
  props.disabled ? 'bg-cp-bg-container-disabled text-cp-text-disabled' : typeClasses[props.type],
])
</script>

<template>
  <span class="inline-flex max-w-full min-w-0 items-center align-middle font-emphasis leading-snug" :class="classes">
    <span :id="closable ? contentId : undefined" class="min-w-0 [overflow-wrap:anywhere]">
      <slot />
    </span>
    <button
      v-if="closable"
      type="button"
      :disabled="disabled"
      :aria-label="closeLabel"
      :aria-labelledby="closeLabel ? undefined : `${closeId} ${contentId}`"
      class="inline-grid size-5 shrink-0 cursor-pointer place-items-center rounded-full border-0 bg-transparent p-0 text-inherit outline-none transition-colors duration-150 hover:bg-current/10 focus-visible:ring-2 focus-visible:ring-cp-control-outline active:bg-current/20 disabled:cursor-not-allowed disabled:hover:bg-transparent motion-reduce:transition-none"
      @click.stop="$emit('close', $event)"
    >
      <span :id="closeId" class="sr-only">移除</span>
      <X class="size-3" aria-hidden="true" />
    </button>
  </span>
</template>
