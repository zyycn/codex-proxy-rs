<script setup lang="ts">
import { computed } from 'vue'

const props = withDefaults(
  defineProps<{
    value: string
    name: string
    label: string
    disabled?: boolean
    showLabel?: boolean
  }>(),
  {
    disabled: false,
    showLabel: false,
  },
)

defineSlots<{
  default?: () => unknown
}>()

const model = defineModel<string>({ required: true })
const checked = computed(() => model.value === props.value)
const indicatorClasses = computed(() => [
  'relative inline-grid size-4 shrink-0 place-items-center rounded-full transition-colors duration-150 motion-reduce:transition-none',
  props.disabled
    ? 'bg-cp-disabled text-cp-disabled-icon'
    : checked.value
      ? 'bg-cp-accent text-cp-accent-on'
      : 'bg-cp-muted text-transparent group-hover:bg-cp-default-active',
])
</script>

<template>
  <!-- The native radio is nested so the full label remains one accessible click target. -->
  <!-- eslint-disable-next-line vue-a11y/label-has-for -->
  <label
    class="group relative inline-flex min-h-4 min-w-4 items-center gap-2.5"
    :class="disabled ? 'cursor-not-allowed opacity-55' : 'cursor-pointer'"
  >
    <input
      v-model="model"
      type="radio"
      class="peer sr-only"
      :name="name"
      :value="value"
      :aria-label="label"
      :disabled="disabled"
    >
    <span
      :class="indicatorClasses"
      class="peer-focus-visible:ring-2 peer-focus-visible:ring-cp-accent-border peer-focus-visible:ring-offset-2 peer-focus-visible:ring-offset-cp-surface"
      aria-hidden="true"
    >
      <span class="size-1.5 rounded-full bg-current" />
    </span>
    <span v-if="$slots.default" class="min-w-0 flex-1">
      <slot />
    </span>
    <span v-else-if="showLabel" class="text-[13px] leading-none font-emphasis">
      {{ label }}
    </span>
  </label>
</template>
