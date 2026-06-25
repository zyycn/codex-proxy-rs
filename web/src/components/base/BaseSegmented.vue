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
  <div class="inline-flex h-9 items-center gap-1 rounded-xl bg-(--cp-bg-muted) p-1" role="tablist">
    <button
      v-for="option in options"
      :key="option.value"
      class="h-7 min-w-0 flex-1 rounded-[9px] border-0 px-3 text-xs leading-none font-[650] transition-colors outline-none focus-visible:ring-2 focus-visible:ring-(--cp-info-border)"
      :class="
        model === option.value
          ? 'bg-white text-(--cp-text-primary) shadow-(--cp-shadow-control)'
          : 'bg-transparent text-(--cp-text-secondary) hover:text-(--cp-text-primary)'
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
