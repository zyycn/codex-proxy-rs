<script setup lang="ts">
import { Minus, Plus } from '@lucide/vue'
import { computed, useAttrs } from 'vue'

import BaseIconButton from './BaseIconButton.vue'

defineOptions({ inheritAttrs: false })

const props = withDefaults(
  defineProps<{
    label: string
    min?: number
    max?: number
    step?: number
    unit?: string
    disabled?: boolean
  }>(),
  {
    min: undefined,
    max: undefined,
    step: 1,
    unit: undefined,
    disabled: false,
  },
)

const model = defineModel<number>({ required: true })
const attrs = useAttrs()

const rootAttrs = computed(() => ({ class: attrs.class, style: attrs.style }))
const controlAttrs = computed(() => Object.fromEntries(
  Object.entries(attrs).filter(([key]) => !['class', 'style'].includes(key)),
))
const canDecrease = computed(() => !props.disabled && (props.min === undefined || model.value > props.min))
const canIncrease = computed(() => !props.disabled && (props.max === undefined || model.value < props.max))

function clampValue(value: number) {
  return Math.min(props.max ?? Number.POSITIVE_INFINITY, Math.max(props.min ?? Number.NEGATIVE_INFINITY, value))
}

function stepPrecision() {
  const decimal = String(props.step).split('.')[1]
  return decimal?.length ?? 0
}

function updateModel(event: Event) {
  const value = (event.target as HTMLInputElement).valueAsNumber
  if (Number.isFinite(value))
    model.value = clampValue(value)
}

function restoreValue(event: Event) {
  const input = event.target as HTMLInputElement
  input.value = String(model.value)
}

function stepBy(direction: -1 | 1) {
  const next = clampValue(model.value + props.step * direction)
  model.value = Number(next.toFixed(stepPrecision()))
}
</script>

<template>
  <div
    v-bind="rootAttrs"
    class="inline-flex h-8 items-center rounded-cp bg-cp-fill-tertiary p-0.5 text-cp-text transition-[background-color,box-shadow] duration-150 focus-within:bg-cp-bg-container focus-within:shadow-cp-input-active motion-reduce:transition-none"
  >
    <BaseIconButton
      :label="`减少${label}`"
      size="sm"
      variant="ghost"
      class="size-7!"
      :disabled="!canDecrease"
      @click="stepBy(-1)"
    >
      <Minus class="size-3" />
    </BaseIconButton>

    <span class="inline-flex min-w-0 items-baseline justify-center gap-1 px-1">
      <input
        v-bind="controlAttrs"
        :value="model"
        type="number"
        inputmode="decimal"
        :aria-label="label"
        :min="min"
        :max="max"
        :step="step"
        :disabled="disabled"
        class="w-9 min-w-0 appearance-[textfield] border-0 bg-transparent p-0 text-right font-mono text-cp-xs font-bold tabular-nums text-cp-text outline-none disabled:text-cp-text-disabled [&::-webkit-inner-spin-button]:m-0 [&::-webkit-inner-spin-button]:appearance-none [&::-webkit-outer-spin-button]:m-0 [&::-webkit-outer-spin-button]:appearance-none"
        @input="updateModel"
        @blur="restoreValue"
      >
      <span
        v-if="unit"
        class="shrink-0 text-[9px] font-emphasis text-cp-text-quaternary"
        aria-hidden="true"
      >
        {{ unit }}
      </span>
    </span>

    <BaseIconButton
      :label="`增加${label}`"
      size="sm"
      variant="ghost"
      class="size-7!"
      :disabled="!canIncrease"
      @click="stepBy(1)"
    >
      <Plus class="size-3" />
    </BaseIconButton>
  </div>
</template>
