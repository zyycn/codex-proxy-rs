<script setup lang="ts">
import { computed } from 'vue'

type InputSize = 'small' | 'default' | 'large'

const props = withDefaults(
  defineProps<{
    placeholder?: string
    type?: string
    disabled?: boolean
    error?: string
    size?: InputSize
    autocomplete?: string
  }>(),
  {
    placeholder: '',
    type: 'text',
    disabled: false,
    error: undefined,
    size: 'default',
    autocomplete: undefined,
  },
)

const model = defineModel<string>({ default: '' })

const sizeClasses = {
  large: 'h-10 px-4 gap-2.5 text-sm rounded-(--cp-control-radius-base)',
  default: 'h-8 px-3 gap-2.5 text-sm rounded-(--cp-control-radius-base)',
  small: 'h-6 px-2 gap-1.5 text-xs rounded-(--cp-control-radius-small)',
}

const containerClasses = computed(() => [
  'inline-flex w-full items-center border transition-all',
  'focus-within:bg-(--cp-bg-surface) focus-within:border-(--cp-info-border) focus-within:shadow-(--cp-shadow-control)',
  sizeClasses[props.size],
  props.disabled
    ? 'border-(--cp-disabled-border) bg-(--cp-disabled-bg)'
    : props.error
      ? 'border-(--cp-danger-border) bg-(--cp-danger-bg)'
      : 'border-transparent bg-(--cp-bg-subtle)',
])

const iconClasses = computed(() => [
  'inline-flex shrink-0',
  props.disabled
    ? 'text-(--cp-disabled-icon)'
    : props.error
      ? 'text-(--cp-danger)'
      : 'text-(--cp-text-muted)',
])

const inputClasses = computed(() => [
  'min-w-0 flex-1 border-0 bg-transparent font-[650] leading-[1.15] outline-0',
  'placeholder:text-(--cp-text-muted) disabled:text-(--cp-disabled-text)',
  props.error
    ? 'text-(--cp-danger-text)'
    : props.disabled
      ? 'text-(--cp-disabled-text)'
      : 'text-(--cp-text-primary)',
])
</script>

<template>
  <label class="grid gap-2">
    <span :class="containerClasses">
      <span v-if="$slots.prefix" :class="iconClasses">
        <slot name="prefix" />
      </span>
      <input
        v-model="model"
        :class="inputClasses"
        :placeholder="placeholder"
        :type="type"
        :disabled="disabled"
        :autocomplete="autocomplete"
      />
      <span v-if="$slots.suffix" :class="iconClasses">
        <slot name="suffix" />
      </span>
    </span>
    <span v-if="error" class="text-xs font-[650] leading-[1.15] text-(--cp-danger-text)">{{
      error
    }}</span>
  </label>
</template>
