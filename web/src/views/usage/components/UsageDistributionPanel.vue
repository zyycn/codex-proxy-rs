<script setup lang="ts">
import { computed } from 'vue'

import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import { formatCostMetric, formatUsageMetric } from '../constants'

const props = defineProps<{
  title: string
  description: string
  nameLabel: string
  items: any[]
  color: string
  sourceOptions?: any[]
  showCostColumn?: boolean
}>()

const source = defineModel<string>('source', { default: '' })

const sortedItems = computed(() =>
  [...(props.items || [])].sort((left, right) => metricValue(right) - metricValue(left)),
)

const totalMetric = computed(() =>
  sortedItems.value.reduce((sum, item) => sum + metricValue(item), 0),
)

const tableGridClass = computed(() =>
  props.showCostColumn === false
    ? 'grid-cols-[128px_64px_76px_76px_76px]'
    : 'grid-cols-[148px_64px_76px_76px_76px_76px]',
)

const tableWidthClass = computed(() =>
  props.showCostColumn === false ? 'min-w-[420px] w-full' : 'min-w-[516px] w-full',
)

const tableMaxHeight = computed(() => {
  const rowCount = Math.min(sortedItems.value.length, 4)
  return `${32 + rowCount * 46}px`
})

const hasData = computed(() =>
  sortedItems.value.some(
    (item) => Number(item.requestCount || 0) > 0 || Number(item.totalTokens || 0) > 0,
  ),
)

const chartColors = computed(() => [
  props.color,
  '--cp-success',
  '--cp-normal',
  '--cp-warning',
  '--cp-text-tertiary',
])

const donutStyle = computed(() => {
  if (!totalMetric.value) {
    return {
      background: 'var(--cp-bg-muted)',
    }
  }

  let cursor = 0
  const stops = sortedItems.value.map((item, index) => {
    const start = cursor
    const size = (metricValue(item) / totalMetric.value) * 100
    cursor += size
    const color = `var(${chartColors.value[index % chartColors.value.length]})`
    return `${color} ${start}% ${cursor}%`
  })

  return {
    background: `conic-gradient(${stops.join(', ')})`,
  }
})

function metricValue(item: any) {
  return Number(item.totalTokens || 0)
}

function markerStyle(index: number) {
  return {
    backgroundColor: `var(${chartColors.value[index % chartColors.value.length]})`,
  }
}

function formatCompactUsage(value: number) {
  const number = Number(value || 0)
  if (number >= 1_000_000_000) return `${trimFixed(number / 1_000_000_000, 1)}B`
  if (number >= 1_000_000) return `${trimFixed(number / 1_000_000, 1)}M`
  if (number >= 10_000) return `${trimFixed(number / 1_000, 1)}K`
  if (number >= 1_000) return `${trimFixed(number / 1_000, 2)}K`
  return new Intl.NumberFormat('zh-CN').format(number)
}

function formatCompactCost(value: number) {
  const number = Number(value || 0)
  if (!number) return '$0'
  if (number >= 1_000_000) return `$${trimFixed(number / 1_000_000, 2)}M`
  if (number >= 1_000) return `$${trimFixed(number / 1_000, 2)}K`
  if (number >= 1) return `$${trimFixed(number, 2)}`
  if (number >= 0.01) return `$${trimFixed(number, 4)}`
  return `$${number.toFixed(6)}`
}

function trimFixed(value: number, digits: number) {
  return value.toFixed(digits).replace(/\.?0+$/, '')
}
</script>

<template>
  <BaseCard
    :padded="false"
    :title="title"
    :description="description"
    header-class="px-5 pt-4"
    body-class="px-5 pt-3 pb-4"
  >
    <template #actions>
      <BaseSegmented
        v-if="sourceOptions?.length"
        v-model="source"
        :options="sourceOptions"
        class="w-42"
      />
    </template>

    <template #body>
      <div
        v-if="hasData"
        class="grid h-70 min-h-0 grid-cols-1 gap-4 lg:h-64 lg:grid-cols-[190px_minmax(0,1fr)]"
      >
        <div class="relative flex h-34 min-h-0 shrink-0 items-center justify-center lg:h-auto">
          <div
            class="size-34 rounded-full transition-transform duration-200 hover:scale-[1.02] lg:size-42"
            :style="donutStyle"
          />
          <div class="absolute size-17 rounded-full bg-(--cp-bg-surface) lg:size-21" />
          <div class="pointer-events-none absolute inset-0 flex items-center justify-center">
            <div class="grid text-center">
              <span class="text-[11px] font-bold text-(--cp-text-muted)"> Token </span>
              <strong
                class="mt-1 font-mono text-[16px] leading-none font-extrabold tabular-nums text-(--cp-text-primary) lg:text-[18px]"
                :title="formatUsageMetric(totalMetric)"
              >
                {{ formatCompactUsage(totalMetric) }}
              </strong>
            </div>
          </div>
        </div>

        <div class="min-h-0 min-w-0 overflow-hidden">
          <BaseScrollbar horizontal :max-height="tableMaxHeight" view-class="pb-2 pr-3">
            <div class="flex min-h-0 flex-col" :class="tableWidthClass">
              <div
                class="grid shrink-0 gap-2 px-3 pb-2 text-[12px] font-bold text-(--cp-text-muted)"
                :class="tableGridClass"
              >
                <span>{{ nameLabel }}</span>
                <span class="text-right">请求</span>
                <span class="text-right">Token</span>
                <span class="text-right">实际</span>
                <span v-if="showCostColumn !== false" class="text-right">成本</span>
                <span class="text-right">标准</span>
              </div>

              <div class="grid gap-1.5">
                <div
                  v-for="(item, index) in sortedItems"
                  :key="item.name"
                  class="grid items-center gap-2 rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 py-2.5 text-[12px] font-[650]"
                  :class="tableGridClass"
                >
                  <div class="min-w-0">
                    <div class="flex min-w-0 items-center gap-2">
                      <i class="size-1.75 shrink-0 rounded-full" :style="markerStyle(index)" />
                      <span class="truncate text-[13px] font-[720] text-(--cp-text-primary)">
                        {{ item.name || '未知' }}
                      </span>
                    </div>
                  </div>
                  <span class="text-right font-mono tabular-nums text-(--cp-text-primary)">
                    {{ formatCompactUsage(item.requestCount) }}
                  </span>
                  <span
                    class="text-right font-mono tabular-nums text-(--cp-text-secondary)"
                    :title="formatUsageMetric(item.totalTokens)"
                  >
                    {{ formatCompactUsage(item.totalTokens) }}
                  </span>
                  <span
                    class="text-right font-mono tabular-nums text-(--cp-success-text)"
                    :title="item.actualCostDisplay || formatCostMetric(item.actualCost)"
                  >
                    {{ formatCompactCost(item.actualCost) }}
                  </span>
                  <span
                    v-if="showCostColumn !== false"
                    class="text-right font-mono tabular-nums text-(--cp-warning-text)"
                    :title="item.accountCostDisplay || formatCostMetric(item.accountCost)"
                  >
                    {{ formatCompactCost(item.accountCost) }}
                  </span>
                  <span
                    class="text-right font-mono tabular-nums text-(--cp-text-secondary)"
                    :title="item.costDisplay || formatCostMetric(item.cost)"
                  >
                    {{ formatCompactCost(item.cost) }}
                  </span>
                </div>
              </div>
            </div>
          </BaseScrollbar>
        </div>
      </div>

      <BaseEmpty
        v-else
        compact
        plain
        title="暂无分布数据"
        description="当前筛选范围还没有可聚合的使用记录。"
        class="h-70 place-content-center lg:h-64"
      />
    </template>
  </BaseCard>
</template>
