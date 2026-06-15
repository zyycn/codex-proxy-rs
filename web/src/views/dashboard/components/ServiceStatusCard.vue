<script setup lang="ts">
import type { SemanticTone, ServiceStatusItem } from '../types'

defineProps<{
  items: ServiceStatusItem[]
}>()

const iconToneClasses: Record<SemanticTone, string> = {
  normal: 'bg-[var(--cp-normal-bg)] text-[var(--cp-normal)]',
  info: 'bg-[var(--cp-info-bg)] text-[var(--cp-info)]',
  success: 'bg-[var(--cp-success-bg)] text-[var(--cp-success)]',
  warning: 'bg-[var(--cp-warning-bg)] text-[var(--cp-warning)]',
  danger: 'bg-[var(--cp-danger-bg)] text-[var(--cp-danger)]',
}

const valueToneClasses: Record<SemanticTone, string> = {
  normal: 'text-[var(--cp-normal-text)]',
  info: 'text-[var(--cp-info-text)]',
  success: 'text-[var(--cp-success-text)]',
  warning: 'text-[var(--cp-warning-text)]',
  danger: 'text-[var(--cp-danger-text)]',
}
</script>

<template>
  <article class="h-[380px] w-[608px] rounded-[18px] bg-white px-7 pt-6 shadow-[var(--cp-shadow-card)]">
    <h2 class="m-0 text-[20px] leading-[1.15] font-[760] text-[var(--cp-text-primary)]">服务状态</h2>

    <div class="mt-[23px] grid gap-2">
      <div
        v-for="item in items"
        :key="item.label"
        class="grid h-[50px] w-[552px] grid-cols-[24px_16px_224px_16px_86px_34px_100px] items-center rounded-[14px] bg-[var(--cp-bg-subtle)] px-3.5"
      >
        <span class="inline-flex size-6 items-center justify-center rounded-lg" :class="iconToneClasses[item.tone]">
          <component :is="item.icon" :size="14" />
        </span>
        <strong class="col-start-3 w-40 text-[13px] leading-[1.15] font-[650] text-[var(--cp-text-primary)]">{{ item.label }}</strong>
        <span class="col-start-5 w-[86px] text-[13px] leading-[1.15] font-bold" :class="valueToneClasses[item.tone]">{{ item.value }}</span>
        <span class="col-start-7 w-[100px] font-mono text-[12px] leading-[1.15] font-semibold text-[var(--cp-text-secondary)]">{{ item.detail }}</span>
      </div>
    </div>
  </article>
</template>
