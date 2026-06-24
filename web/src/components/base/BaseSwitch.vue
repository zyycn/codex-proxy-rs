<script setup lang="ts">
import { computed } from 'vue'

const props = withDefaults(
  defineProps<{
    disabled?: boolean
    label?: string
  }>(),
  {
    disabled: false,
    label: undefined,
  },
)

const model = defineModel<boolean>({ default: false })

const trackClasses = computed(() => [
  'relative inline-flex h-6 w-10.5 shrink-0 rounded-full border-0 transition-colors duration-180 outline-none',
  props.disabled
    ? 'cursor-not-allowed bg-(--cp-disabled-bg) opacity-70'
    : model.value
      ? 'cursor-pointer bg-(--cp-info)'
      : 'cursor-pointer bg-(--cp-bg-muted)',
])

const knobClasses = computed(() => [
  'absolute top-0.75 left-0 size-4.5 rounded-full bg-(--cp-bg-surface) shadow-(--cp-shadow-control) transition-transform duration-180',
  model.value ? 'translate-x-5' : 'translate-x-0.75',
])

function toggle() {
  if (props.disabled) {
    return
  }

  model.value = !model.value
}
</script>

<template>
  <button
    type="button"
    :class="trackClasses"
    role="switch"
    :aria-checked="model"
    :aria-label="label"
    :disabled="disabled"
    @click="toggle"
  >
    <span :class="knobClasses" />
  </button>
</template>
