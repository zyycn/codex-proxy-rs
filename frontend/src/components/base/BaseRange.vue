<script setup lang="ts">
import type { CSSProperties } from 'vue'
import { computed } from 'vue'

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
    min: 0,
    max: 100,
    step: 1,
    unit: undefined,
    disabled: false,
  },
)

const model = defineModel<number>({ required: true })

const rangeStyle = computed<CSSProperties>(() => {
  const span = props.max - props.min
  const progress = span > 0 ? ((model.value - props.min) / span) * 100 : 0
  return {
    '--base-range-progress': `${Math.min(100, Math.max(0, progress))}%`,
  }
})

const valueText = computed(() => `${model.value}${props.unit ?? ''}`)

function updateModel(event: Event) {
  const value = (event.target as HTMLInputElement).valueAsNumber
  if (Number.isFinite(value))
    model.value = value
}
</script>

<template>
  <input
    :value="model"
    type="range"
    :min="min"
    :max="max"
    :step="step"
    :disabled="disabled"
    :aria-label="label"
    :aria-valuetext="valueText"
    class="base-range h-5 w-full"
    :style="rangeStyle"
    @input="updateModel"
  >
</template>

<style scoped>
.base-range {
  appearance: none;
  cursor: pointer;
  background: transparent;
  outline: none;
}

.base-range::-webkit-slider-runnable-track {
  height: 4px;
  border-radius: 999px;
  background: linear-gradient(
    to right,
    var(--cp-color-primary) 0 var(--base-range-progress),
    var(--cp-color-fill-secondary) var(--base-range-progress) 100%
  );
}

.base-range::-webkit-slider-thumb {
  width: 14px;
  height: 14px;
  margin-top: -5px;
  appearance: none;
  border: 2px solid var(--cp-color-bg-container);
  border-radius: 999px;
  background: var(--cp-color-primary);
  transition:
    box-shadow 150ms ease,
    transform 150ms ease;
}

.base-range::-moz-range-track {
  height: 4px;
  border-radius: 999px;
  background: var(--cp-color-fill-secondary);
}

.base-range::-moz-range-progress {
  height: 4px;
  border-radius: 999px;
  background: var(--cp-color-primary);
}

.base-range::-moz-range-thumb {
  width: 10px;
  height: 10px;
  border: 2px solid var(--cp-color-bg-container);
  border-radius: 999px;
  background: var(--cp-color-primary);
  transition:
    box-shadow 150ms ease,
    transform 150ms ease;
}

.base-range:hover::-webkit-slider-thumb {
  transform: scale(1.08);
}

.base-range:hover::-moz-range-thumb {
  transform: scale(1.08);
}

.base-range:focus-visible::-webkit-slider-thumb {
  box-shadow: 0 0 0 3px var(--cp-color-primary-bg-hover);
}

.base-range:focus-visible::-moz-range-thumb {
  box-shadow: 0 0 0 3px var(--cp-color-primary-bg-hover);
}

.base-range:disabled {
  cursor: not-allowed;
  opacity: 0.5;
}

@media (prefers-reduced-motion: reduce) {
  .base-range::-webkit-slider-thumb {
    transition: none;
  }

  .base-range::-moz-range-thumb {
    transition: none;
  }
}
</style>
