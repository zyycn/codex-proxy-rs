<script setup lang="ts">
import { computed } from 'vue'

type TextareaSize = 'sm' | 'md' | 'lg'

const props = withDefaults(
  defineProps<{
    placeholder?: string
    disabled?: boolean
    error?: string
    size?: TextareaSize
  }>(),
  {
    placeholder: '',
    disabled: false,
    error: undefined,
    size: 'md',
  },
)

const model = defineModel<string>({ default: '' })

const sizeClasses: Record<TextareaSize, string> = {
  sm: 'h-28',
  md: 'h-40',
  lg: 'h-56',
}

const textareaClasses = computed(() => [
  'w-full resize-none rounded-(--cp-input-radius-base) border-0 bg-[var(--cp-input-current-bg,var(--cp-input-context-bg))] px-3.5 py-3 text-(--cp-text-primary) shadow-(--cp-shadow-input) outline-none transition-[background-color,box-shadow,color] duration-160 placeholder:text-(--cp-text-muted)',
  'hover:bg-[var(--cp-input-current-bg-hover,var(--cp-input-context-bg-hover))] hover:shadow-(--cp-shadow-input-hover) focus:bg-(--cp-input-soft-bg-focus) focus:shadow-(--cp-shadow-input-focus)',
  'disabled:cursor-not-allowed disabled:bg-(--cp-disabled-bg) disabled:text-(--cp-disabled-text) disabled:shadow-none',
  sizeClasses[props.size],
  'text-[13px] leading-[1.55] font-[650]',
  props.error ? 'bg-(--cp-input-error-soft-bg) shadow-(--cp-shadow-input-error)' : undefined,
])
</script>

<template>
  <label class="grid box-content gap-2 overflow-visible p-0.75">
    <textarea
      v-model="model"
      :class="textareaClasses"
      :placeholder="placeholder"
      :disabled="disabled"
      :aria-invalid="error ? 'true' : undefined"
    />
    <span v-if="error" class="text-xs leading-[1.15] font-[650] text-(--cp-danger-text)">
      {{ error }}
    </span>
  </label>
</template>
