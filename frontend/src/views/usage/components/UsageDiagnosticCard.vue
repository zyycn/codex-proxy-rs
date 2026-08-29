<script setup lang="ts">
import type { getUsageRecordInsightsDiagnostics } from '@/api'
import { CornerDownRight } from '@lucide/vue'

import { computed } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { formatLocalizedCompactNumber as formatCompactNumber } from '@/utils/number'

import { formatDuration, formatPercent, formatUsd } from '../utils/format'

type Diagnostics = Awaited<ReturnType<typeof getUsageRecordInsightsDiagnostics>>

const props = withDefaults(
  defineProps<{
    diagnostics: Diagnostics
    loading?: boolean
  }>(),
  {
    loading: false,
  },
)

const dimension = defineModel('dimension', { type: String, required: true })

const dimensionOptions = [
  { label: '模型', value: 'model' },
  { label: '账号', value: 'account' },
  { label: '密钥', value: 'apiKey' },
  { label: '上游', value: 'provider' },
  { label: '传输', value: 'transport' },
  { label: '错误', value: 'failureClass' },
]

const diagnosticColumns = defineTableColumns<DiagnosticDisplayItem>([
  {
    key: 'nameDisplay',
    label: '维度',
    kind: 'custom',
    size: 'xl',
  },
  {
    key: 'requestCount',
    label: '请求 / 占比',
    kind: 'numeric',
  },
  {
    key: 'errorCount',
    label: '错误 / 错误率',
    kind: 'numeric',
  },
  { key: 'latencyP95Ms', label: 'P95', kind: 'numeric', size: 'sm' },
  {
    key: 'estimatedCost',
    label: '预估费用',
    kind: 'numeric',
  },
])

const selectedDimensionLabel = computed(
  () => dimensionOptions.find(option => option.value === dimension.value)?.label ?? '维度',
)

const resultDimension = computed(() => props.diagnostics.dimension || dimension.value)
const resultDimensionLabel = computed(
  () => dimensionOptions.find(option => option.value === resultDimension.value)?.label ?? '维度',
)

const sortedItems = computed(() =>
  [...props.diagnostics.items].sort(
    (left, right) => right.errorCount - left.errorCount || right.requestCount - left.requestCount,
  ),
)

type DiagnosticDisplayItem = Diagnostics['items'][number] & {
  nameDisplay: ReturnType<typeof diagnosticNameDisplay>
}

const displayItems = computed<DiagnosticDisplayItem[]>(() =>
  sortedItems.value.map(item => ({
    ...item,
    nameDisplay: diagnosticNameDisplay(item.name),
  })),
)
const hasData = computed(() => !props.loading && displayItems.value.length > 0)

function diagnosticNameDisplay(name: string) {
  const raw = name.trim() || '未知'
  const full
    = resultDimension.value === 'transport' ? ({ websocket: 'WS', http_sse: 'SSE' }[raw] ?? raw) : raw
  if (resultDimension.value !== 'model' && resultDimension.value !== 'account') {
    return { primary: full, secondary: '', full }
  }

  const [primary, secondary = ''] = full.split(/\s+(?:→|->)\s+/, 2)
  return {
    primary: primary || full,
    secondary,
    full,
  }
}
</script>

<template>
  <BaseCard
    as="article"
    title="热点诊断"
    :description="`按${selectedDimensionLabel}定位错误、慢请求与费用热点`"
    class="h-105 min-h-105 max-h-105 min-w-0 w-full lg:h-full lg:min-h-90 lg:max-h-105"
  >
    <template #actions>
      <BaseSegmented
        v-model="dimension"
        label="诊断维度"
        :options="dimensionOptions"
        :disabled="loading"
        class="w-full min-w-0 lg:w-80"
      />
    </template>

    <template #body>
      <BaseTable
        v-if="hasData"
        :key="resultDimension"
        class="min-h-0 w-full xl:contain-[size]"
        :columns="diagnosticColumns"
        :rows="displayItems"
        density="compact"
        row-key="key"
        empty-text="暂无诊断数据"
      >
        <template #header-nameDisplay>
          {{ resultDimensionLabel }}
        </template>

        <template #nameDisplay="{ row }">
          <div class="inline-grid max-w-full min-w-0 gap-1" :title="row.nameDisplay.full">
            <code
              class="block max-w-full truncate font-mono text-cp-sm leading-none font-heavy text-cp-text"
            >
              {{ row.nameDisplay.primary }}
            </code>
            <div
              v-if="row.nameDisplay.secondary"
              class="flex min-w-0 items-center gap-1.25 text-cp-text-secondary"
            >
              <CornerDownRight
                v-if="resultDimension !== 'account'"
                class="size-3.25 shrink-0 text-cp-blue-text"
                stroke-width="2.4"
              />
              <code class="block truncate font-mono text-cp-xs leading-none font-bold">
                {{ row.nameDisplay.secondary }}
              </code>
            </div>
          </div>
        </template>

        <template #requestCount="{ row }">
          <span
            class="grid justify-items-end gap-1 font-mono leading-none tabular-nums"
            :title="`成功 ${formatCompactNumber(row.successCount)}`"
          >
            <strong class="font-bold text-cp-text">
              {{ formatCompactNumber(row.requestCount) }}
            </strong>
            <small class="text-[10px] font-emphasis text-cp-text-quaternary">
              {{ formatPercent(row.requestShare) }}
            </small>
          </span>
        </template>

        <template #errorCount="{ row }">
          <span class="grid justify-items-end gap-1 font-mono leading-none tabular-nums">
            <strong
              class="font-bold"
              :class="row.errorCount > 0 ? 'text-cp-error-text' : 'text-cp-text'"
            >
              {{ formatCompactNumber(row.errorCount) }}
            </strong>
            <small
              class="text-[10px] font-emphasis"
              :class="row.errorRate > 0 ? 'text-cp-error-text' : 'text-cp-text-quaternary'"
            >
              {{ formatPercent(row.errorRate) }}
            </small>
          </span>
        </template>

        <template #latencyP95Ms="{ row }">
          <span class="font-mono font-bold tabular-nums text-cp-orange-text">
            {{ formatDuration(row.latencyP95Ms) }}
          </span>
        </template>

        <template #estimatedCost="{ row }">
          <span class="font-mono font-bold tabular-nums text-cp-green-text">
            {{ formatUsd(row.estimatedCost) }}
          </span>
        </template>
      </BaseTable>
      <BaseEmpty
        v-else
        size="sm"
        surface="none"
        :title="loading ? '正在加载热点诊断数据' : '暂无诊断数据'"
        description="当前范围没有可诊断的请求记录"
        class="h-full place-content-center"
      />
    </template>
  </BaseCard>
</template>
