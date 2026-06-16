<script setup lang="ts">
interface SegmentedOption {
  label: string
  value: string
}

defineProps<{
  options: SegmentedOption[]
}>()

const model = defineModel<string>({ required: true })
</script>

<template>
  <div class="inline-flex min-h-9 gap-1 rounded-xl bg-(--cp-bg-muted) p-1" role="tablist">
    <button
      v-for="option in options"
      :key="option.value"
      class="min-w-18.5 rounded-[9px] border-0 bg-transparent px-3 text-xs font-bold leading-none text-(--cp-text-secondary) transition-colors"
      :class="model === option.value ? 'bg-(--cp-bg-surface) text-(--cp-text-primary) shadow-(--cp-shadow-control)' : ''"
      type="button"
      role="tab"
      :aria-selected="model === option.value"
      @click="model = option.value"
    >
      {{ option.label }}
    </button>
  </div>
</template>
