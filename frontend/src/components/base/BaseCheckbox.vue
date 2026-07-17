<script setup lang="ts">
import { Check, Minus } from '@lucide/vue'
import { computed } from 'vue'

const props = withDefaults(
  defineProps<{
    indeterminate?: boolean
    disabled?: boolean
    label?: string
  }>(),
  {
    indeterminate: false,
    disabled: false,
    label: undefined,
  },
)

const model = defineModel<boolean>({ default: false })

const checked = computed(() => model.value || props.indeterminate)

const boxClasses = computed(() => [
  'relative inline-flex size-4 shrink-0 items-center justify-center rounded-(--cp-checkbox-radius) border-0 transition-[background-color,box-shadow,color] duration-150',
  props.disabled
    ? 'bg-(--cp-disabled-bg) text-(--cp-disabled-icon)'
    : checked.value
      ? 'bg-(--cp-info-bg) text-(--cp-info)'
      : 'bg-(--cp-bg-surface) text-transparent shadow-[inset_0_0_0_1px_var(--cp-default-border-hover)]',
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
    class="group inline-flex min-h-4 min-w-4 appearance-none items-center gap-2.5 border-0 bg-transparent p-0 text-[13px] leading-none font-[650] transition-opacity outline-none focus-visible:ring-2 focus-visible:ring-(--cp-info-border) focus-visible:ring-offset-2 focus-visible:ring-offset-(--cp-bg-surface)"
    :class="disabled ? 'cursor-not-allowed opacity-55' : 'cursor-pointer'"
    role="checkbox"
    :aria-checked="indeterminate ? 'mixed' : model"
    :aria-label="label"
    :disabled="disabled"
    @click="toggle"
  >
    <span :class="boxClasses">
      <Minus
        class="absolute transition-opacity duration-150 size-3"
        :class="[indeterminate ? 'opacity-100' : 'opacity-0']"
      />
      <Check
        class="absolute transition-opacity duration-150 size-3"
        :class="[!indeterminate && model ? 'opacity-100' : 'opacity-0']"
      />
    </span>
  </button>
</template>
