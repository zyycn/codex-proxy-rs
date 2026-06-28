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

const buttonClasses = computed(() => [
  'group inline-flex rounded-full border-0 bg-transparent p-0 outline-none transition-[background-color,box-shadow,opacity] duration-180',
  'focus-visible:ring-2 focus-visible:ring-(--cp-info-border) focus-visible:ring-offset-2 focus-visible:ring-offset-(--cp-bg-surface)',
  props.disabled ? 'cursor-not-allowed opacity-70' : 'cursor-pointer',
])

const trackClasses = computed(() => [
  'relative inline-flex h-6 w-11 shrink-0 items-center rounded-full p-0.5 transition-[background-color,box-shadow] duration-180',
  props.disabled
    ? 'bg-(--cp-disabled-bg) shadow-none'
    : model.value
      ? 'bg-(--cp-info) shadow-(--cp-shadow-control) group-hover:bg-(--cp-info-hover)'
      : 'bg-(--cp-bg-muted) shadow-(--cp-shadow-control) group-hover:bg-(--cp-default-bg-active)',
])

const thumbClasses = computed(() => [
  'size-5 rounded-full bg-(--cp-bg-surface) shadow-(--cp-shadow-control) transition-transform duration-180 ease-out',
  model.value ? 'translate-x-5' : 'translate-x-0',
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
    role="switch"
    :aria-checked="model"
    :aria-label="label"
    :disabled="disabled"
    :class="buttonClasses"
    @click="toggle"
  >
    <span :class="trackClasses">
      <span :class="thumbClasses" />
    </span>
  </button>
</template>
