<script setup lang="ts">
import { Check, Minus } from '@lucide/vue'
import { computed, useTemplateRef, watchEffect } from 'vue'

const props = withDefaults(
  defineProps<{
    label: string
    indeterminate?: boolean
    disabled?: boolean
    showLabel?: boolean
  }>(),
  {
    indeterminate: false,
    disabled: false,
    showLabel: false,
  },
)

const model = defineModel<boolean>({ default: false })
const inputRef = useTemplateRef<HTMLInputElement>('input')
const checked = computed(() => model.value || props.indeterminate)

watchEffect(() => {
  if (inputRef.value)
    inputRef.value.indeterminate = props.indeterminate
})

const boxClasses = computed(() => [
  'relative inline-flex size-4 shrink-0 items-center justify-center rounded-cp-sm border-0 transition-[background-color,box-shadow,color] duration-150 motion-reduce:transition-none',
  props.disabled
    ? 'bg-cp-bg-container-disabled text-cp-text-disabled'
    : checked.value
      ? 'bg-cp-primary text-(--cp-button-primary-color)'
      : 'bg-cp-bg-container text-transparent shadow-[inset_0_0_0_1px_var(--cp-color-border)]',
])
</script>

<template>
  <!-- The native checkbox is nested; the rule does not resolve this Vue template pattern. -->
  <!-- eslint-disable-next-line vue-a11y/label-has-for -->
  <label
    class="group relative inline-flex min-h-4 min-w-4 items-center gap-2.5 text-cp leading-none font-emphasis"
    :class="disabled ? 'cursor-not-allowed opacity-55' : 'cursor-pointer'"
  >
    <input
      ref="input"
      v-model="model"
      type="checkbox"
      class="peer sr-only"
      :aria-label="label"
      :disabled="disabled"
    >
    <span
      :class="boxClasses"
      class="peer-focus-visible:ring-2 peer-focus-visible:ring-cp-control-outline peer-focus-visible:ring-offset-2 peer-focus-visible:ring-offset-cp-bg-container"
      aria-hidden="true"
    >
      <Minus
        class="absolute size-3 transition-opacity duration-150 motion-reduce:transition-none"
        :class="indeterminate ? 'opacity-100' : 'opacity-0'"
      />
      <Check
        class="absolute size-3 transition-opacity duration-150 motion-reduce:transition-none"
        :class="!indeterminate && model ? 'opacity-100' : 'opacity-0'"
      />
    </span>
    <span v-if="showLabel" class="text-cp leading-none font-emphasis">{{ label }}</span>
  </label>
</template>
