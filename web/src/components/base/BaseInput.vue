<script setup lang="ts">
import { computed } from 'vue'

type InputSize = 'small' | 'default' | 'large'

const props = withDefaults(defineProps<{
  placeholder?: string
  disabled?: boolean
  error?: string
  size?: InputSize
}>(), {
  placeholder: '',
  disabled: false,
  error: undefined,
  size: 'default',
})

const model = defineModel<string>({ default: '' })

const sizeClasses = {
  large: 'h-10 px-4 gap-2.5 text-[14px] rounded-[4px]',
  default: 'h-8 px-3 gap-2.5 text-[14px] rounded-[4px]',
  small: 'h-6 px-2 gap-1.5 text-[12px] rounded-[2px]',
}

const containerClasses = computed(() => [
  'inline-flex w-full items-center border border-transparent transition-all',
  'focus-within:bg-white focus-within:shadow-[0_8px_18px_-16px_rgba(14,23,38,0.08)]',
  sizeClasses[props.size],
  props.disabled ? 'bg-[#F1F5F9]' : props.error ? 'bg-[#FEF2F2]' : 'bg-[#F8FAFC]',
])

const iconClasses = computed(() => [
  'inline-flex shrink-0',
  props.disabled ? 'text-[#CBD5E1]' : props.error ? 'text-[#EF4444]' : 'text-[#94A3B8]',
])

const inputClasses = computed(() => [
  'min-w-0 flex-1 border-0 bg-transparent font-[650] leading-[1.15] outline-0',
  'placeholder:text-[#94A3B8] disabled:text-[#94A3B8]',
  props.error ? 'text-[#B91C1C]' : props.disabled ? 'text-[#94A3B8]' : 'text-[#0E1726]',
])
</script>

<template>
  <label class="grid gap-2">
    <span :class="containerClasses">
      <span v-if="$slots.prefix" :class="iconClasses">
        <slot name="prefix" />
      </span>
      <input v-model="model" :class="inputClasses" :placeholder="placeholder" :disabled="disabled" />
      <span v-if="$slots.suffix" :class="iconClasses">
        <slot name="suffix" />
      </span>
    </span>
    <span v-if="error" class="text-[12px] font-[650] leading-[1.15] text-[#B91C1C]">{{ error }}</span>
  </label>
</template>
