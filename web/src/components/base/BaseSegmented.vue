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
  <div class="inline-flex min-h-9 gap-1 rounded-xl bg-[var(--cp-bg-muted)] p-1" role="tablist">
    <button
      v-for="option in options"
      :key="option.value"
      class="min-w-[74px] rounded-[9px] border-0 bg-transparent px-3 text-xs font-bold leading-none text-[var(--cp-text-secondary)] transition-colors"
      :class="model === option.value ? 'bg-[var(--cp-bg-surface)] text-[var(--cp-text-primary)] shadow-[var(--cp-shadow-control)]' : ''"
      type="button"
      role="tab"
      :aria-selected="model === option.value"
      @click="model = option.value"
    >
      {{ option.label }}
    </button>
  </div>
</template>
