<script setup lang="ts">
import { clamp } from 'es-toolkit'
import { computed } from 'vue'
import type { Component } from 'vue'

interface SegmentedOption {
  label: string
  value: string
  icon?: Component
}

const props = withDefaults(
  defineProps<{
    options: SegmentedOption[]
    disabled?: boolean
  }>(),
  {
    disabled: false,
  },
)

const model = defineModel<string>({ required: true })

const activeIndex = computed(() => {
  const index = props.options.findIndex((option) => option.value === model.value)
  return index >= 0 ? index : 0
})

const optionCount = computed(() => clamp(props.options.length, 1, Number.POSITIVE_INFINITY))

const gridStyle = computed(() => ({
  gridTemplateColumns: `repeat(${optionCount.value}, minmax(0, 1fr))`,
}))

const indicatorStyle = computed(() => ({
  width: `calc((100% - 8px) / ${optionCount.value})`,
  transform: `translateX(${activeIndex.value * 100}%)`,
}))

function selectOption(value: string) {
  if (props.disabled || value === model.value) return
  model.value = value
}
</script>

<template>
  <div
    class="relative inline-grid h-9 items-center rounded-(--cp-icon-button-radius) bg-(--cp-bg-muted) p-1"
    :style="gridStyle"
    role="tablist"
  >
    <span
      class="pointer-events-none absolute top-1 bottom-1 left-1 rounded-(--cp-input-radius-base) bg-(--cp-bg-surface) shadow-(--cp-shadow-control) transition-transform duration-200 ease-out"
      :style="indicatorStyle"
    />
    <button
      v-for="option in options"
      :key="option.value"
      class="relative z-10 inline-flex h-7 min-w-0 items-center justify-center gap-1.5 rounded-(--cp-input-radius-base) border-0 bg-transparent px-3 text-xs leading-none font-[650] transition-colors duration-150 outline-none focus-visible:ring-2 focus-visible:ring-(--cp-info-border)"
      :class="[
        model === option.value
          ? 'text-(--cp-text-primary)'
          : 'text-(--cp-text-secondary) hover:text-(--cp-text-primary)',
        disabled ? 'cursor-not-allowed opacity-60 hover:text-(--cp-text-secondary)' : undefined,
      ]"
      type="button"
      role="tab"
      :aria-selected="model === option.value"
      :disabled="disabled"
      @click="selectOption(option.value)"
    >
      <component :is="option.icon" v-if="option.icon" class="size-3.5 shrink-0" />
      {{ option.label }}
    </button>
  </div>
</template>
