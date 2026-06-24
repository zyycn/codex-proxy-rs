<script setup lang="ts">
import { ChevronDown } from '@lucide/vue'
import { computed } from 'vue'

interface SelectOption {
  label: string
  value: string
  disabled?: boolean
}

type SelectSize = 'small' | 'default' | 'large'

const props = withDefaults(
  defineProps<{
    options: SelectOption[]
    size?: SelectSize
    disabled?: boolean
  }>(),
  {
    size: 'default',
    disabled: false,
  },
)

const model = defineModel<string>({ required: true })

const sizeClasses: Record<SelectSize, string> = {
  large: 'h-10 text-sm',
  default: 'h-8 text-[13px]',
  small: 'h-6 text-xs',
}

const triggerClasses = computed(() => [
  'relative inline-flex w-full rounded-(--cp-control-radius-base) border transition-all',
  'focus-within:border-(--cp-info-border) focus-within:bg-(--cp-bg-surface) focus-within:shadow-(--cp-shadow-control)',
  sizeClasses[props.size],
  props.disabled
    ? 'border-(--cp-disabled-border) bg-(--cp-disabled-bg) text-(--cp-disabled-text)'
    : 'border-transparent bg-(--cp-bg-surface) text-(--cp-text-primary)',
])
</script>

<template>
  <label :class="triggerClasses">
    <select
      v-model="model"
      class="h-full w-full appearance-none rounded-[inherit] border-0 bg-transparent pr-8 pl-3 font-semibold text-current outline-0 disabled:text-(--cp-disabled-text)"
      :disabled="disabled"
    >
      <option
        v-for="option in options"
        :key="option.value"
        :value="option.value"
        :disabled="option.disabled"
      >
        {{ option.label }}
      </option>
    </select>
    <ChevronDown
      class="pointer-events-none absolute right-3 top-1/2 -translate-y-1/2"
      :class="disabled ? 'text-(--cp-disabled-icon)' : 'text-(--cp-text-secondary)'"
      :size="size === 'small' ? 14 : 16"
    />
  </label>
</template>
