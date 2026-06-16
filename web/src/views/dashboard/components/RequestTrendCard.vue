<script setup lang="ts">
import BaseCard from '../../../components/base/BaseCard.vue'
import type { SemanticTone, TrendPoint, TrendSummaryItem } from '../types'

defineProps<{
  points: TrendPoint[]
  summary: TrendSummaryItem[]
}>()

const tabs = ['请求', '错误', '延迟']
const axisLabels = ['00', '04', '08', '12', '16', '20', '24']

const dotToneClasses: Record<SemanticTone, string> = {
  normal: 'bg-(--cp-normal)',
  info: 'bg-(--cp-info)',
  success: 'bg-(--cp-success)',
  warning: 'bg-(--cp-warning)',
  danger: 'bg-(--cp-danger)',
}
</script>

<template>
  <BaseCard as="article" :padded="false" class="h-95 w-full px-7 pt-5.5">
    <header class="flex items-start justify-between">
      <div class="pt-0.5">
        <h2 class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)">请求趋势</h2>
        <p class="mt-1.75 mb-0 text-[13px] leading-[1.15] font-[650] text-(--cp-text-secondary)">最近 24 小时 · 14:00 峰值 15.2K</p>
      </div>

      <div class="grid h-9.5 w-61.5 grid-cols-3 gap-1 rounded-xl bg-(--cp-bg-muted) p-1">
        <button
          v-for="tab in tabs"
          :key="tab"
          class="h-7.5 rounded-[9px] border-0 text-xs leading-[1.15] font-[650]"
          :class="tab === '请求' ? 'bg-white text-(--cp-text-primary) shadow-[0_10px_24px_-18px_#0E172614]' : 'bg-transparent text-(--cp-text-secondary)'"
          type="button"
        >
          {{ tab }}
        </button>
      </div>
    </header>

    <div class="mt-4.75 grid grid-cols-[minmax(0,1fr)_minmax(150px,180px)] gap-7.5">
      <div class="h-67 w-full overflow-hidden rounded-[10px] bg-white">
        <svg class="block h-67 w-full" viewBox="0 0 682 268" preserveAspectRatio="none" role="img" aria-label="最近 24 小时请求趋势">
          <g transform="translate(0 0)">
            <line v-for="y in [18, 62, 106, 150, 194, 238]" :key="y" x1="0" x2="650" :y1="y" :y2="y" stroke="#F1F5F9" />
            <path
              d="M0 150c70-14 96-20 136-38 48-24 74-20 108-58 42-42 78-22 116 6 44 32 82 18 124-10 44-24 82-16 120 30 18 22 34 12 46-6"
              fill="none"
              stroke="#2563EB"
              stroke-linecap="round"
              stroke-linejoin="round"
              stroke-width="3"
              transform="translate(0 34) scale(1 0.838983)"
            />
            <path
              d="M0 70c80-2 122-8 170-6 56 2 116-14 166-8 62 6 94 18 142 2 48-16 108-2 172-10"
              fill="none"
              stroke="#EF4444"
              stroke-linecap="round"
              stroke-linejoin="round"
              stroke-width="2"
              transform="translate(0 126) scale(1 0.813559)"
            />
            <path
              d="M0 44c72 4 116-8 174-4 58 2 110-6 166-2 66 8 118-8 174-4 60 6 100-4 136-8"
              fill="none"
              stroke="#0F9F9A"
              stroke-linecap="round"
              stroke-linejoin="round"
              stroke-width="2"
              transform="translate(0 140) scale(1 0.791667)"
            />
          </g>
          <g font-family="JetBrains Mono Variable, JetBrains Mono, SFMono-Regular, Consolas, monospace" font-size="11" font-weight="700" fill="#94A3B8">
            <text v-for="(label, index) in axisLabels" :key="label" :x="[0, 108, 216, 324, 432, 540, 632][index]" y="261">{{ label }}</text>
          </g>
        </svg>
      </div>

      <aside class="flex h-67 w-full flex-col rounded-2xl bg-(--cp-bg-subtle) px-5 py-4.5" style="gap: 36.6px">
        <div
          v-for="item in summary"
          :key="item.label"
          class="grid grid-cols-[minmax(0,1fr)_8px] items-start gap-x-3 gap-y-px"
        >
          <span class="col-span-2 text-xs leading-[1.15] font-bold text-(--cp-text-secondary)">{{ item.label }}</span>
          <strong class="mt-1.75 font-mono text-2xl leading-[1.15] font-[760] tabular-nums text-(--cp-text-primary)">{{ item.value }}</strong>
          <i class="mt-3.5 size-2 justify-self-end rounded-full" :class="dotToneClasses[item.tone]" />
        </div>
      </aside>
    </div>
  </BaseCard>
</template>
