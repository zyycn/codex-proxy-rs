<script setup lang="ts">
import type { rotationOptions } from '../constants'
import BaseCard from '@/components/base/BaseCard.vue'

type RotationOption = (typeof rotationOptions)[number]
type RotationStrategy = RotationOption['value']

defineProps<{
  options: readonly RotationOption[]
}>()

const model = defineModel<RotationStrategy | ''>({ required: true })
</script>

<template>
  <BaseCard
    title="调度策略"
    description="决定每次请求如何调度账号池"
  >
    <div class="grid max-w-6xl gap-3 lg:grid-cols-4">
      <button
        v-for="option in options"
        :key="option.value"
        type="button"
        class="min-h-25 cursor-pointer rounded-cp-control border-0 px-4 py-3.5 text-left shadow-cp-input outline-none transition-[background-color,box-shadow,color] duration-160 focus-visible:ring-2 focus-visible:ring-cp-info-border"
        :class="
          model === option.value
            ? 'bg-cp-info-bg text-cp-info-text shadow-cp-control'
            : 'bg-(--cp-input-current-bg,var(--cp-input-context-bg)) text-cp-primary hover:bg-(--cp-input-current-bg-hover,var(--cp-input-context-bg-hover)) hover:shadow-cp-input-hover'
        "
        :aria-pressed="model === option.value"
        @click="model = option.value"
      >
        <span class="flex items-center gap-2">
          <span
            class="inline-flex size-4 shrink-0 items-center justify-center rounded-full bg-cp-surface shadow-[inset_0_0_0_1px_var(--cp-default-border-hover)]"
          >
            <span
              class="size-2 rounded-full transition-opacity duration-150"
              :class="model === option.value ? 'bg-cp-info opacity-100' : 'opacity-0'"
            />
          </span>
          <span class="text-[14px] leading-[1.15] font-heavy">{{ option.label }}</span>
        </span>
        <span class="mt-2 block text-[13px] leading-normal font-emphasis text-cp-secondary">
          {{ option.description }}
        </span>
      </button>
    </div>
  </BaseCard>
</template>
