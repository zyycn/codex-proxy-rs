<script setup lang="ts">
import BaseCard from '@/components/base/BaseCard.vue'

type RotationStrategy = 'least_used' | 'round_robin' | 'sticky'

interface RotationOption {
  label: string
  value: RotationStrategy
  description: string
}

defineProps<{
  options: RotationOption[]
}>()

const model = defineModel<RotationStrategy>({ required: true })
</script>

<template>
  <BaseCard
    :padded="false"
    title="账号选择"
    description="决定每次请求如何使用账号池。"
    header-class="px-5 pt-4"
    body-class="px-5 py-5"
  >
    <div class="grid max-w-6xl gap-3 lg:grid-cols-3">
      <button
        v-for="option in options"
        :key="option.value"
        type="button"
        class="min-h-25 cursor-pointer rounded-(--cp-input-radius-base) border-0 px-4 py-3.5 text-left shadow-(--cp-shadow-input) outline-none transition-[background-color,box-shadow,color] duration-160 focus-visible:ring-2 focus-visible:ring-(--cp-info-border)"
        :class="
          model === option.value
            ? 'bg-(--cp-info-bg) text-(--cp-info-text) shadow-(--cp-shadow-control)'
            : 'bg-(--cp-input-current-bg,var(--cp-input-context-bg)) text-(--cp-text-primary) hover:bg-(--cp-input-current-bg-hover,var(--cp-input-context-bg-hover)) hover:shadow-(--cp-shadow-input-hover)'
        "
        :aria-pressed="model === option.value"
        @click="model = option.value"
      >
        <span class="flex items-center gap-2">
          <span
            class="inline-flex size-4 shrink-0 items-center justify-center rounded-full bg-(--cp-bg-surface) shadow-[inset_0_0_0_1px_var(--cp-default-border-hover)]"
          >
            <span
              class="size-2 rounded-full transition-opacity duration-150"
              :class="model === option.value ? 'bg-(--cp-info) opacity-100' : 'opacity-0'"
            />
          </span>
          <span class="text-[14px] leading-[1.15] font-[760]">{{ option.label }}</span>
        </span>
        <span class="mt-2 block text-[13px] leading-normal font-[650] text-(--cp-text-secondary)">
          {{ option.description }}
        </span>
      </button>
    </div>
  </BaseCard>
</template>
