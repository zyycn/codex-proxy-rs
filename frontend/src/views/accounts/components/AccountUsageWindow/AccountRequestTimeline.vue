<script setup lang="ts">
import type { AccountRequestBar } from './presenter'

withDefaults(
  defineProps<{
    bars: AccountRequestBar[]
    label: string
    showNativeTooltip?: boolean
  }>(),
  {
    showNativeTooltip: true,
  },
)
</script>

<template>
  <div
    class="flex items-stretch gap-px overflow-hidden rounded-full"
    role="img"
    :aria-label="label"
    :title="showNativeTooltip ? label : undefined"
  >
    <span
      v-for="bar in bars"
      :key="bar.key"
      class="relative min-w-0 flex-1 bg-cp-border-secondary"
      :title="showNativeTooltip ? bar.title : undefined"
    >
      <span
        class="absolute inset-x-0 bottom-0 bg-cp-success"
        :style="{ height: bar.height }"
      />
    </span>
  </div>
</template>
