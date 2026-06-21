<script setup lang="ts">
import { computed, ref, shallowRef } from 'vue'
import type { EChartsOption } from 'echarts'
import BaseCard from '../../../components/base/BaseCard.vue'
import BaseChart from '../../../components/charts/BaseChart.vue'
import type { TrendPoint, TrendSummaryItem } from '../types'

const props = defineProps<{
  points: TrendPoint[]
  summary: TrendSummaryItem[]
}>()

const tabs = ['用量', '延迟', '错误']
const activeTab = ref('用量')

const chartOption = computed<EChartsOption>(() => {
  const times = props.points.map(p => `${p.time}:00`)
  const getValues = () => {
    if (activeTab.value === '用量') return props.points.map(p => p.tokens)
    if (activeTab.value === '延迟') return props.points.map(p => p.latency)
    return props.points.map(p => p.errors)
  }
  const values = getValues()
  const colors = { '用量': '#2563EB', '延迟': '#0F9F9A', '错误': '#EF4444' }
  const color = colors[activeTab.value as keyof typeof colors]
  return {
    grid: { left: 30, right: 0, top: 8, bottom: 24 },
    xAxis: { type: 'category', data: times, axisLabel: { color: '#94A3B8', fontSize: 10, fontFamily: 'JetBrains Mono' }, axisLine: { show: false }, axisTick: { show: false } },
    yAxis: { type: 'value', splitLine: { lineStyle: { color: '#F1F5F9' } }, axisLabel: { show: false } },
    series: [{
      type: 'line', data: values, smooth: true, symbol: 'none', lineStyle: { color, width: 2.5 },
      areaStyle: { color: { type: 'linear', x: 0, y: 0, x2: 0, y2: 1, colorStops: [{ offset: 0, color: color + '18' }, { offset: 1, color: color + '02' }] } },
    }],
    tooltip: {
      trigger: 'axis',
      backgroundColor: '#fff',
      borderColor: 'transparent',
      borderWidth: 0,
      padding: [10, 14],
      textStyle: { color: '#334155', fontSize: 12, fontFamily: 'Inter, system-ui, sans-serif', fontWeight: 600 },
      extraCssText: 'border-radius: 12px; box-shadow: 0 4px 20px rgba(0,0,0,0.08);',
      axisPointer: { type: 'line', lineStyle: { color: '#E2E8F0', type: 'dashed' } },
    },
  }
})

const activeSummary = computed(() => {
  const pts = props.points
  if (!pts.length) return []
  if (activeTab.value === '用量') {
    const totalTokens = pts.reduce((s, p) => s + p.tokens, 0)
    return [
      { label: '输入', value: formatTokens(Math.floor(totalTokens * 0.45)), tone: 'info' as const },
      { label: '输出', value: formatTokens(Math.floor(totalTokens * 0.55)), tone: 'success' as const },
      { label: '缓存', value: formatTokens(0), tone: 'normal' as const },
    ]
  }
  if (activeTab.value === '延迟') {
    const withLatency = pts.filter(p => p.latency > 0)
    const avg = withLatency.length ? Math.round(withLatency.reduce((s, p) => s + p.latency, 0) / withLatency.length) : 0
    const max = withLatency.length ? Math.max(...withLatency.map(p => p.latency)) : 0
    const min = withLatency.length ? Math.min(...withLatency.map(p => p.latency)) : 0
    return [
      { label: '平均', value: avg ? `${avg}ms` : '—', tone: 'info' as const },
      { label: '最高', value: max ? `${max}ms` : '—', tone: 'warning' as const },
      { label: '最低', value: min ? `${min}ms` : '—', tone: 'success' as const },
    ]
  }
  const totalErrors = pts.reduce((s, p) => s + p.errors, 0)
  const totalRequests = pts.reduce((s, p) => s + p.requests, 0)
  const errorRate = totalRequests > 0 ? ((totalErrors / totalRequests) * 100).toFixed(1) : '0'
  return [
    { label: '错误数', value: String(totalErrors), tone: totalErrors > 0 ? 'danger' as const : 'normal' as const },
    { label: '成功率', value: totalRequests > 0 ? `${(100 - Number(errorRate)).toFixed(1)}%` : '—', tone: 'success' as const },
    { label: '总请求', value: String(totalRequests), tone: 'info' as const },
  ]
})

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`
  return String(n)
}
</script>

<template>
  <BaseCard as="article" :padded="false" class="h-95 w-full px-7 pt-5.5">
    <header class="flex items-start justify-between">
      <div class="pt-0.5">
        <h2 class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)">请求趋势</h2>
        <p class="mt-1.75 mb-0 text-[13px] leading-[1.15] font-[650] text-(--cp-text-secondary)">最近 24 小时</p>
      </div>

      <div class="grid h-9.5 w-61.5 grid-cols-3 gap-1 rounded-xl bg-(--cp-bg-muted) p-1">
        <button
          v-for="tab in tabs"
          :key="tab"
          class="h-7.5 rounded-[9px] border-0 text-xs leading-[1.15] font-[650] cursor-pointer transition-colors"
          :class="activeTab === tab ? 'bg-white text-(--cp-text-primary) shadow-[0_10px_24px_-18px_#0E172614]' : 'bg-transparent text-(--cp-text-secondary)'"
          type="button"
          @click="activeTab = tab"
        >
          {{ tab }}
        </button>
      </div>
    </header>

    <div class="mt-4.75 grid grid-cols-[minmax(0,1fr)_minmax(150px,180px)] gap-7.5">
      <div class="h-67 w-full overflow-hidden rounded-[10px] bg-white">
        <BaseChart :option="chartOption" :height="268" />
      </div>

      <aside class="flex h-67 w-full flex-col rounded-2xl bg-(--cp-bg-subtle) px-5 py-4.5" style="gap: 36.6px">
        <div
          v-for="item in activeSummary"
          :key="item.label"
          class="grid grid-cols-[minmax(0,1fr)_8px] items-start gap-x-3 gap-y-px"
        >
          <span class="col-span-2 text-xs leading-[1.15] font-bold text-(--cp-text-secondary)">{{ item.label }}</span>
          <strong class="mt-1.75 font-mono text-2xl leading-[1.15] font-[760] tabular-nums text-(--cp-text-primary)">{{ item.value }}</strong>
          <i class="mt-3.5 size-2 justify-self-end rounded-full" :class="{
            'bg-(--cp-info)': item.tone === 'info',
            'bg-(--cp-success)': item.tone === 'success',
            'bg-(--cp-warning)': item.tone === 'warning',
            'bg-(--cp-danger)': item.tone === 'danger',
            'bg-(--cp-normal)': item.tone === 'normal',
          }" />
        </div>
      </aside>
    </div>
  </BaseCard>
</template>
