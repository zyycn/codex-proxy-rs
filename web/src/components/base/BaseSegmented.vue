<script setup lang="ts">
import { computed } from 'vue'

interface SegmentedOption {
  label: string
  value: string
}

const props = defineProps<{
  options: SegmentedOption[]
}>()

const model = defineModel<string>({ required: true })

const activeIndex = computed(() => {
  const index = props.options.findIndex((option) => option.value === model.value)
  return index >= 0 ? index : 0
})

const gridStyle = computed(() => ({
  gridTemplateColumns: `repeat(${Math.max(props.options.length, 1)}, minmax(0, 1fr))`,
}))

const indicatorStyle = computed(() => ({
  width: `calc((100% - 8px) / ${Math.max(props.options.length, 1)})`,
  transform: `translateX(${activeIndex.value * 100}%)`,
}))
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
      class="relative z-10 h-7 min-w-0 rounded-(--cp-input-radius-base) border-0 bg-transparent px-3 text-xs leading-none font-[650] transition-colors duration-150 outline-none focus-visible:ring-2 focus-visible:ring-(--cp-info-border)"
      :class="
        model === option.value
          ? 'text-(--cp-text-primary)'
          : 'text-(--cp-text-secondary) hover:text-(--cp-text-primary)'
      "
      type="button"
      role="tab"
      :aria-selected="model === option.value"
      @click="model = option.value"
    >
      {{ option.label }}
    </button>
  </div>
</template>
