<script setup lang="ts">
import { Check, Minus } from '@lucide/vue'
import { computed } from 'vue'

type CheckboxSize = 'default' | 'table'

const props = withDefaults(
  defineProps<{
    indeterminate?: boolean
    disabled?: boolean
    label?: string
    showLabel?: boolean
    size?: CheckboxSize
  }>(),
  {
    indeterminate: false,
    disabled: false,
    label: undefined,
    showLabel: false,
    size: 'default',
  },
)

const model = defineModel<boolean>({ default: false })

const checked = computed(() => model.value || props.indeterminate)
const boxSizeClass = computed(() => (props.size === 'table' ? 'size-4' : 'size-[18px]'))
const rootSizeClass = computed(() =>
  props.size === 'table' ? 'min-h-4 min-w-4' : 'min-h-[18px] min-w-[18px]',
)
const iconSizeClass = computed(() => (props.size === 'table' ? 'size-3' : 'size-[13px]'))

const boxClasses = computed(() => [
  'relative inline-flex shrink-0 items-center justify-center rounded-(--cp-checkbox-radius) border-0 transition-[background-color,box-shadow,color] duration-150',
  boxSizeClass.value,
  props.disabled
    ? 'bg-(--cp-disabled-bg) text-(--cp-disabled-icon)'
    : checked.value
      ? 'bg-(--cp-info-bg) text-(--cp-info)'
      : props.size === 'table'
        ? 'bg-(--cp-bg-surface) text-transparent shadow-[inset_0_0_0_1px_#CBD5E1]'
        : 'bg-(--cp-bg-surface) text-transparent shadow-(--cp-shadow-control)',
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
    class="group inline-flex appearance-none items-center gap-2.5 border-0 bg-transparent p-0 text-[13px] font-[650] leading-none outline-none transition-opacity focus-visible:ring-2 focus-visible:ring-(--cp-info-border) focus-visible:ring-offset-2 focus-visible:ring-offset-(--cp-bg-surface)"
    :class="[rootSizeClass, disabled ? 'cursor-not-allowed opacity-55' : 'cursor-pointer']"
    role="checkbox"
    :aria-checked="indeterminate ? 'mixed' : model"
    :aria-label="label"
    :disabled="disabled"
    @click="toggle"
  >
    <span :class="boxClasses">
      <Minus
        class="absolute transition-opacity duration-150"
        :class="[iconSizeClass, indeterminate ? 'opacity-100' : 'opacity-0']"
      />
      <Check
        class="absolute transition-opacity duration-150"
        :class="[iconSizeClass, !indeterminate && model ? 'opacity-100' : 'opacity-0']"
      />
    </span>
    <span v-if="$slots.default || (label && showLabel)" class="text-(--cp-text-primary)">
      <slot>{{ label }}</slot>
    </span>
  </button>
</template>
