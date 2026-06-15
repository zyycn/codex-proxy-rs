<script setup lang="ts">
import type { MetricCardItem, SemanticTone } from '../types'

defineProps<{
  metric: MetricCardItem
}>()

const iconToneClasses: Record<SemanticTone, string> = {
  normal: 'bg-[var(--cp-normal-bg)] text-[var(--cp-normal)]',
  info: 'bg-[var(--cp-info-bg)] text-[var(--cp-info)]',
  success: 'bg-[var(--cp-success-bg)] text-[var(--cp-success)]',
  warning: 'bg-[var(--cp-warning-bg)] text-[var(--cp-warning)]',
  danger: 'bg-[var(--cp-danger-bg)] text-[var(--cp-danger)]',
}

const detailToneClasses: Record<SemanticTone, string> = {
  normal: 'text-[var(--cp-normal-text)]',
  info: 'text-[var(--cp-info-text)]',
  success: 'text-[var(--cp-success-text)]',
  warning: 'text-[var(--cp-warning-text)]',
  danger: 'text-[var(--cp-danger-text)]',
}
</script>

<template>
  <article class="h-[154px] w-[378px] rounded-2xl bg-white px-6 pt-5 shadow-[var(--cp-shadow-card)]">
    <div class="flex items-start gap-3">
      <span class="inline-flex size-[34px] shrink-0 items-center justify-center rounded-[10px]" :class="iconToneClasses[metric.tone]">
        <component :is="metric.icon" :size="18" />
      </span>
      <span class="mt-1 text-[13px] leading-[1.15] font-[650] text-[var(--cp-text-secondary)]">{{ metric.title }}</span>
    </div>

    <strong class="mt-2.5 block w-[330px] font-mono text-[28px] leading-[1.05] font-[780] tabular-nums text-[var(--cp-text-primary)]">
      {{ metric.value }}
    </strong>

    <div class="mt-[16.6px] grid h-[30px] w-[330px] grid-cols-[40px_82px_48px_94px] items-center gap-0 rounded-[10px] bg-[var(--cp-bg-subtle)] px-3">
      <span class="text-[11px] leading-[1.15] font-[650] text-[var(--cp-text-muted)]">{{ metric.details[0]?.label }}</span>
      <b class="font-mono text-[12px] leading-[1.15] font-bold tabular-nums" :class="metric.details[0]?.tone ? detailToneClasses[metric.details[0].tone] : undefined">
        {{ metric.details[0]?.value }}
      </b>
      <span class="text-[11px] leading-[1.15] font-[650] text-[var(--cp-text-muted)]">{{ metric.details[1]?.label }}</span>
      <b class="text-right font-mono text-[12px] leading-[1.15] font-bold tabular-nums" :class="metric.details[1]?.tone ? detailToneClasses[metric.details[1].tone] : undefined">
        {{ metric.details[1]?.value }}
      </b>
    </div>
  </article>
</template>
