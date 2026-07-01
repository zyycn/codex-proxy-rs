<script setup lang="ts">
import { CornerDownRight } from '@lucide/vue'
import { computed } from 'vue'

import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'

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
const totalMetricText = computed(() => sortedItems.value[0]?.totalTokensTotal ?? '0')

const tableGridClass = computed(() =>
  props.showCostColumn === false
    ? 'grid-cols-[minmax(128px,1fr)_repeat(4,minmax(66px,0.75fr))]'
    : 'grid-cols-[minmax(128px,1fr)_repeat(5,minmax(64px,0.75fr))]',
)

const tableWidthClass = computed(() =>
  props.showCostColumn === false ? 'min-w-[414px] w-full' : 'min-w-[492px] w-full',
)

const hasData = computed(() =>
  sortedItems.value.some(
    (item) => Number(item.requestCountValue || 0) > 0 || Number(item.totalTokensValue || 0) > 0,
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
  return Number(item.totalTokensValue || 0)
}

function distributionNameParts(name: unknown) {
  const text = typeof name === 'string' && name.trim() ? name.trim() : '未知'
  const [primary, secondary] = text.split(' -> ', 2).map((part) => part.trim())
  return {
    primary: primary || text,
    secondary: secondary || '',
    full: text,
  }
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
        class="w-56"
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
          <div class="absolute size-18 rounded-full bg-(--cp-bg-surface) lg:size-23" />
          <div class="pointer-events-none absolute inset-0 flex items-center justify-center">
            <div class="grid text-center">
              <span class="text-[11px] font-bold text-(--cp-text-muted)">Token</span>
              <strong
                class="mt-1 font-mono text-[16px] leading-none font-extrabold tabular-nums text-(--cp-text-primary) lg:text-[18px]"
                :title="totalMetricText"
              >
                {{ totalMetricText }}
              </strong>
            </div>
          </div>
        </div>

        <div class="min-h-0 min-w-0 overflow-hidden">
          <BaseScrollbar horizontal view-class="pb-2">
            <div class="flex min-h-0 flex-col" :class="tableWidthClass">
              <div
                class="grid shrink-0 gap-2 px-3 pb-2 text-[12px] font-bold text-(--cp-text-muted)"
                :class="tableGridClass"
              >
                <span>{{ nameLabel }}</span>
                <span class="text-center">请求</span>
                <span class="text-center">Token</span>
                <span class="text-center">实际</span>
                <span v-if="showCostColumn !== false" class="text-center">成本</span>
                <span class="text-center">标准</span>
              </div>

              <div class="grid gap-1.5">
                <div
                  v-for="item in sortedItems"
                  :key="item.name"
                  class="grid items-center gap-2 rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 py-2.5 text-[12px] font-[650]"
                  :class="tableGridClass"
                >
                  <div class="min-w-0">
                    <div class="flex min-w-0 items-start">
                      <div
                        class="inline-grid min-w-0 gap-1"
                        :title="distributionNameParts(item.name).full"
                      >
                        <code
                          class="block max-w-full truncate font-mono text-[12px] leading-none font-[760] text-(--cp-text-primary)"
                        >
                          {{ distributionNameParts(item.name).primary }}
                        </code>
                        <div
                          v-if="distributionNameParts(item.name).secondary"
                          class="flex min-w-0 items-center gap-1.25 text-(--cp-text-secondary)"
                        >
                          <CornerDownRight
                            class="size-3.25 shrink-0 text-(--cp-info)"
                            stroke-width="2.4"
                          />
                          <code
                            class="block truncate font-mono text-[11px] leading-none font-[700]"
                          >
                            {{ distributionNameParts(item.name).secondary }}
                          </code>
                        </div>
                      </div>
                    </div>
                  </div>
                  <span class="text-center font-mono tabular-nums text-(--cp-text-primary)">
                    {{ item.requestCount }}
                  </span>
                  <span
                    class="text-center font-mono tabular-nums text-(--cp-text-secondary)"
                    :title="item.totalTokens"
                  >
                    {{ item.totalTokens }}
                  </span>
                  <span
                    class="text-center font-mono tabular-nums text-(--cp-success-text)"
                    :title="item.actualCost"
                  >
                    {{ item.actualCost }}
                  </span>
                  <span
                    v-if="showCostColumn !== false"
                    class="text-center font-mono tabular-nums text-(--cp-warning-text)"
                    :title="item.accountCost"
                  >
                    {{ item.accountCost }}
                  </span>
                  <span
                    class="text-center font-mono tabular-nums text-(--cp-text-secondary)"
                    :title="item.cost"
                  >
                    {{ item.cost }}
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
