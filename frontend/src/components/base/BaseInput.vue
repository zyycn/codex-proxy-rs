<script setup lang="ts">
import { computed } from 'vue'

const props = withDefaults(
  defineProps<{
    placeholder?: string
    type?: string
    disabled?: boolean
    error?: string
    autocomplete?: string
  }>(),
  {
    placeholder: '',
    type: 'text',
    disabled: false,
    error: undefined,
    autocomplete: undefined,
  },
)

const model = defineModel<string>({ default: '' })

const containerClasses = computed(() => [
  'relative inline-flex h-(--cp-input-height-default) w-full min-w-0 items-center gap-2.5 overflow-visible rounded-(--cp-input-radius-base) border-0 px-3.5 text-[13px] text-(--cp-text-primary) shadow-(--cp-shadow-input) transition-[background-color,box-shadow,color] duration-[160ms]',
  props.disabled
    ? 'cursor-not-allowed bg-(--cp-disabled-bg) text-(--cp-disabled-text) shadow-none'
    : props.error
      ? 'bg-(--cp-input-error-soft-bg) shadow-(--cp-shadow-input-error)'
      : [
          'bg-[var(--cp-input-current-bg,var(--cp-input-context-bg))]',
          'hover:bg-[var(--cp-input-current-bg-hover,var(--cp-input-context-bg-hover))] hover:shadow-(--cp-shadow-input-hover)',
          'focus-within:bg-(--cp-input-soft-bg-focus) focus-within:shadow-(--cp-shadow-input-focus)',
        ],
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
  'h-full min-w-0 flex-1 border-0 bg-transparent text-[13px] font-[650] leading-[1.15] outline-0',
  'placeholder:text-(--cp-text-muted) disabled:cursor-not-allowed disabled:text-(--cp-disabled-text)',
  props.error
    ? 'text-(--cp-danger-text)'
    : props.disabled
      ? 'text-(--cp-disabled-text)'
      : 'text-(--cp-text-primary)',
])
</script>

<template>
  <label class="grid box-content gap-2 overflow-visible p-0.75">
    <span class="base-input__control" :class="containerClasses">
      <span v-if="$slots.prefix" :class="iconClasses">
        <slot name="prefix" />
      </span>
      <input
        v-model="model"
        class="base-input__field"
        :class="inputClasses"
        :placeholder="placeholder"
        :type="type"
        :disabled="disabled"
        :autocomplete="autocomplete"
        :aria-invalid="error ? 'true' : undefined"
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

<style scoped>
.base-input__control {
  --base-input-autofill-bg: var(--cp-input-current-bg, var(--cp-input-context-bg));
}

.base-input__control:hover {
  --base-input-autofill-bg: var(--cp-input-current-bg-hover, var(--cp-input-context-bg-hover));
}

.base-input__control:focus-within {
  --base-input-autofill-bg: var(--cp-input-soft-bg-focus);
}

.base-input__field:-webkit-autofill,
.base-input__field:-webkit-autofill:hover,
.base-input__field:-webkit-autofill:focus,
.base-input__field:autofill {
  caret-color: var(--cp-text-primary);
  -webkit-text-fill-color: var(--cp-text-primary) !important;
  -webkit-box-shadow: 0 0 0 1000px var(--base-input-autofill-bg) inset !important;
  box-shadow: 0 0 0 1000px var(--base-input-autofill-bg) inset !important;
}
</style>
