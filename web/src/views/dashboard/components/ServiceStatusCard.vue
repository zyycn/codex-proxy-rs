<script setup lang="ts">
import BaseCard from '../../../components/base/BaseCard.vue'
import type { SemanticTone, ServiceStatusItem } from '../types'

defineProps<{
  items: ServiceStatusItem[]
}>()

const iconToneClasses: Record<SemanticTone, string> = {
  normal: 'bg-(--cp-normal-bg) text-(--cp-normal)',
  info: 'bg-(--cp-info-bg) text-(--cp-info)',
  success: 'bg-(--cp-success-bg) text-(--cp-success)',
  warning: 'bg-(--cp-warning-bg) text-(--cp-warning)',
  danger: 'bg-(--cp-danger-bg) text-(--cp-danger)',
}

const valueToneClasses: Record<SemanticTone, string> = {
  normal: 'text-(--cp-normal-text)',
  info: 'text-(--cp-info-text)',
  success: 'text-(--cp-success-text)',
  warning: 'text-(--cp-warning-text)',
  danger: 'text-(--cp-danger-text)',
}
</script>

<template>
  <BaseCard as="article" :padded="false" class="h-95 w-full px-7 pt-6">
    <h2 class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)">服务状态</h2>

    <div class="mt-5.75 grid gap-2">
      <div
        v-for="item in items"
        :key="item.label"
        class="grid h-12.5 w-full grid-cols-[24px_16px_minmax(0,224fr)_16px_86px_34px_100px] items-center rounded-[14px] bg-(--cp-bg-subtle) px-3.5"
      >
        <span class="inline-flex size-6 items-center justify-center rounded-lg" :class="iconToneClasses[item.tone]">
          <component :is="item.icon" :size="14" />
        </span>
        <strong class="col-start-3 w-40 text-[13px] leading-[1.15] font-[650] text-(--cp-text-primary)">{{ item.label }}</strong>
        <span class="col-start-5 w-21.5 text-[13px] leading-[1.15] font-bold" :class="valueToneClasses[item.tone]">{{ item.value }}</span>
        <span class="col-start-7 w-25 font-mono text-xs leading-[1.15] font-semibold text-(--cp-text-secondary)">{{ item.detail }}</span>
      </div>
    </div>
  </BaseCard>
</template>
