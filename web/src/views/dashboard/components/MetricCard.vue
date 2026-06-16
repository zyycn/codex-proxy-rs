<script setup lang="ts">
import BaseCard from '../../../components/base/BaseCard.vue'
import type { MetricCardItem, SemanticTone } from '../types'

defineProps<{
  metric: MetricCardItem
}>()

const iconToneClasses: Record<SemanticTone, string> = {
  normal: 'bg-(--cp-normal-bg) text-(--cp-normal)',
  info: 'bg-(--cp-info-bg) text-(--cp-info)',
  success: 'bg-(--cp-success-bg) text-(--cp-success)',
  warning: 'bg-(--cp-warning-bg) text-(--cp-warning)',
  danger: 'bg-(--cp-danger-bg) text-(--cp-danger)',
}

const detailToneClasses: Record<SemanticTone, string> = {
  normal: 'text-(--cp-normal-text)',
  info: 'text-(--cp-info-text)',
  success: 'text-(--cp-success-text)',
  warning: 'text-(--cp-warning-text)',
  danger: 'text-(--cp-danger-text)',
}
</script>

<template>
  <BaseCard as="article" :padded="false" radius-class="rounded-2xl" class="h-[154px] w-full px-6 pt-5">
    <div class="flex items-start gap-3">
      <span class="inline-flex size-[34px] shrink-0 items-center justify-center rounded-[10px]" :class="iconToneClasses[metric.tone]">
        <component :is="metric.icon" :size="18" />
      </span>
      <span class="mt-1 text-[13px] leading-[1.15] font-[650] text-(--cp-text-secondary)">{{ metric.title }}</span>
    </div>

    <strong class="mt-2.5 block w-full font-mono text-[28px] leading-[1.05] font-[780] tabular-nums text-(--cp-text-primary)">
      {{ metric.value }}
    </strong>

    <div class="mt-[16.6px] grid h-[30px] w-full grid-cols-2 items-center rounded-[10px] bg-(--cp-bg-subtle) px-3">
      <span class="inline-flex min-w-0 w-full items-center justify-start gap-3">
        <span class="shrink-0 text-[11px] leading-[1.15] font-[650] text-(--cp-text-muted)">{{ metric.details[0]?.label }}</span>
        <b class="min-w-0 truncate font-mono text-xs leading-[1.15] font-bold tabular-nums" :class="metric.details[0]?.tone ? detailToneClasses[metric.details[0].tone] : undefined">
          {{ metric.details[0]?.value }}
        </b>
      </span>
      <span class="inline-flex min-w-0 w-full items-center justify-start gap-3">
        <span class="shrink-0 text-[11px] leading-[1.15] font-[650] text-(--cp-text-muted)">{{ metric.details[1]?.label }}</span>
        <b class="min-w-0 truncate font-mono text-xs leading-[1.15] font-bold tabular-nums" :class="metric.details[1]?.tone ? detailToneClasses[metric.details[1].tone] : undefined">
          {{ metric.details[1]?.value }}
        </b>
      </span>
    </div>
  </BaseCard>
</template>
