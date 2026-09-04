<script setup lang="ts">
import { computed } from 'vue'

const props = withDefaults(
  defineProps<{
    label: string
    disabled?: boolean
    showLabel?: boolean
    activeText?: string
    inactiveText?: string
    inlinePrompt?: boolean
    width?: string | number
  }>(),
  {
    disabled: false,
    showLabel: false,
    activeText: '',
    inactiveText: '',
    inlinePrompt: false,
    width: '',
  },
)

const model = defineModel<boolean>({ default: false })
const hasStateText = computed(() => Boolean(props.activeText || props.inactiveText))
const inlineText = computed(() => model.value ? props.activeText : props.inactiveText)
const trackStyle = computed(() => props.width === ''
  ? undefined
  : {
      width: typeof props.width === 'number' || /^\d+(?:\.\d+)?$/.test(props.width)
        ? `${props.width}px`
        : props.width,
    })

const trackClasses = computed(() => [
  'relative inline-flex h-6 min-w-11 shrink-0 items-center rounded-full p-0.5 transition-[background-color,box-shadow] duration-180 motion-reduce:transition-none',
  props.disabled
    ? 'bg-cp-bg-container-disabled shadow-none'
    : model.value
      ? 'bg-cp-primary shadow-cp-tertiary'
      : 'bg-cp-fill-tertiary shadow-cp-tertiary',
])
const thumbClasses = computed(() => [
  'absolute top-0.5 z-10 size-5 rounded-full bg-cp-white shadow-cp-tertiary transition-[left] duration-180 ease-out motion-reduce:transition-none',
  model.value ? 'left-[calc(100%-1.375rem)]' : 'left-0.5',
])
const inlineTextClasses = computed(() => [
  'pointer-events-none absolute inset-0 flex min-w-0 select-none items-center justify-center overflow-hidden text-[10px] leading-none font-heavy transition-[padding,color] duration-180 motion-reduce:transition-none',
  model.value ? 'text-cp-white' : 'text-cp-text-secondary',
  model.value ? 'pr-6 pl-1' : 'pr-1 pl-6',
])
const inactiveTextClasses = computed(() => [
  'text-cp leading-none font-emphasis transition-colors duration-150 motion-reduce:transition-none',
  model.value ? 'text-cp-text-secondary' : 'text-cp-primary-text',
])
const activeTextClasses = computed(() => [
  'text-cp leading-none font-emphasis transition-colors duration-150 motion-reduce:transition-none',
  model.value ? 'text-cp-primary-text' : 'text-cp-text-secondary',
])
</script>

<template>
  <!-- The native checkbox is nested; the rule does not resolve this Vue template pattern. -->
  <!-- eslint-disable-next-line vue-a11y/label-has-for -->
  <label
    class="relative inline-flex items-center gap-2.5"
    :class="disabled ? 'cursor-not-allowed opacity-70' : 'cursor-pointer'"
  >
    <span v-if="inactiveText && !inlinePrompt" :class="inactiveTextClasses">
      {{ inactiveText }}
    </span>
    <input
      v-model="model"
      type="checkbox"
      role="switch"
      class="peer sr-only"
      :aria-label="label"
      :disabled="disabled"
    >
    <span
      :class="trackClasses"
      :style="trackStyle"
      class="peer-focus-visible:ring-2 peer-focus-visible:ring-cp-control-outline peer-focus-visible:ring-offset-2 peer-focus-visible:ring-offset-cp-bg-container"
    >
      <span v-if="inlinePrompt && inlineText" :class="inlineTextClasses">
        <span class="min-w-0 truncate">{{ inlineText }}</span>
      </span>
      <span :class="thumbClasses" />
    </span>
    <span v-if="activeText && !inlinePrompt" :class="activeTextClasses">{{ activeText }}</span>
    <span v-else-if="showLabel && !hasStateText" class="text-cp font-emphasis text-cp-text">
      {{ label }}
    </span>
  </label>
</template>
