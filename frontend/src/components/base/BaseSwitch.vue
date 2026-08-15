<script setup lang="ts">
import { computed } from 'vue'

const props = withDefaults(
  defineProps<{
    label: string
    disabled?: boolean
    showLabel?: boolean
  }>(),
  {
    disabled: false,
    showLabel: false,
  },
)

const model = defineModel<boolean>({ default: false })

const trackClasses = computed(() => [
  'relative inline-flex h-6 w-11 shrink-0 items-center rounded-full p-0.5 transition-[background-color,box-shadow] duration-180 motion-reduce:transition-none',
  props.disabled
    ? 'bg-cp-disabled shadow-none'
    : model.value
      ? 'bg-cp-accent shadow-cp-control group-hover:bg-cp-accent-hover'
      : 'bg-cp-muted shadow-cp-control group-hover:bg-cp-default-active',
])
</script>

<template>
  <!-- The native checkbox is nested; the rule does not resolve this Vue template pattern. -->
  <!-- eslint-disable-next-line vue-a11y/label-has-for -->
  <label
    class="group relative inline-flex items-center gap-2.5"
    :class="disabled ? 'cursor-not-allowed opacity-70' : 'cursor-pointer'"
  >
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
      class="peer-focus-visible:ring-2 peer-focus-visible:ring-cp-accent-border peer-focus-visible:ring-offset-2 peer-focus-visible:ring-offset-cp-surface"
      aria-hidden="true"
    >
      <span
        class="size-5 rounded-full bg-cp-surface shadow-cp-control transition-transform duration-180 ease-out motion-reduce:transition-none"
        :class="model ? 'translate-x-5' : 'translate-x-0'"
      />
    </span>
    <span v-if="showLabel" class="text-[13px] font-emphasis text-cp-primary">{{ label }}</span>
  </label>
</template>
