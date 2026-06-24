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
  <div class="inline-flex h-9 gap-1 rounded-xl bg-(--cp-bg-muted) p-1" role="tablist">
    <button
      v-for="option in options"
      :key="option.value"
      class="min-w-18.5 rounded-[9px] border-0 bg-transparent px-3 text-xs font-[650] leading-none text-(--cp-text-secondary) transition-colors outline-none hover:text-(--cp-text-primary) focus-visible:ring-2 focus-visible:ring-(--cp-info-border)"
      :class="
        model === option.value
          ? 'bg-(--cp-bg-surface) text-(--cp-text-primary) shadow-(--cp-shadow-control)'
          : ''
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
