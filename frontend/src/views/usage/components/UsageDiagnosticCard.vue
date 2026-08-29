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
    label: '请求',
    kind: 'numeric',
    size: 'sm',
  },
  {
    key: 'impactScore',
    label: '风险分',
    kind: 'numeric',
    size: 'sm',
  },
  {
    key: 'errorCount',
    label: '错误 / 未完',
    kind: 'numeric',
    size: 'sm',
  },
  { key: 'firstTokenP95Ms', label: '性能', kind: 'numeric', size: 'lg' },
  {
    key: 'estimatedCost',
    label: '费用',
    kind: 'numeric',
    size: 'sm',
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
    (left, right) => right.impactScore - left.impactScore || right.requestCount - left.requestCount,
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
    :description="`按${selectedDimensionLabel}定位高影响请求`"
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
            <span
              v-if="row.nameDisplay.secondary"
              class="flex min-w-0 items-center gap-1.25 text-cp-text-secondary"
            >
              <CornerDownRight
                v-if="resultDimension !== 'account'"
                class="size-3.25 shrink-0 text-cp-blue-text"
                stroke-width="2.4"
              />
              <code class="block truncate font-mono text-cp-xs font-bold">
                {{ row.nameDisplay.secondary }}
              </code>
            </span>
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

        <template #impactScore="{ row }">
          <strong
            class="font-mono font-bold tabular-nums"
            :class="row.impactScore > 0.1 ? 'text-cp-error-text' : 'text-cp-text-secondary'"
            title="综合错误、未完成、重试、请求占比与 TTFT 的风险分（0–100）"
          >
            {{ formatCompactNumber(row.impactScore * 100) }}
          </strong>
        </template>

        <template #errorCount="{ row }">
          <div
            class="flex items-center justify-end gap-1.5 whitespace-nowrap text-right font-mono leading-none tabular-nums"
            :aria-label="`错误 ${formatCompactNumber(row.errorCount)}，未完成 ${formatCompactNumber(row.nonCompletionCount)}`"
            :title="`错误率 ${formatPercent(row.errorRate)}；未完成率 ${formatPercent(row.nonCompletionRate)}`"
          >
            <strong :class="row.errorCount > 0 ? 'text-cp-error-text' : 'text-cp-text-quaternary'">
              {{ formatCompactNumber(row.errorCount) }}
            </strong>
            <span class="text-[9px] text-cp-text-quaternary">/</span>
            <strong :class="row.nonCompletionCount > 0 ? 'text-cp-orange-text' : 'text-cp-text-quaternary'">
              {{ formatCompactNumber(row.nonCompletionCount) }}
            </strong>
          </div>
        </template>

        <template #firstTokenP95Ms="{ row }">
          <div
            class="grid justify-items-end gap-1 font-mono leading-none tabular-nums"
            :title="`重试率 ${formatPercent(row.retryRate)}`"
          >
            <span class="inline-flex items-baseline gap-1.5">
              <small class="text-[10px] font-emphasis text-cp-text-quaternary">TTFT</small>
              <strong class="font-bold text-cp-blue-text">
                {{ formatDuration(row.firstTokenP95Ms) }}
              </strong>
            </span>
            <small class="flex items-center gap-2 text-[10px] font-emphasis text-cp-text-quaternary">
              <span>P95 {{ formatDuration(row.latencyP95Ms) }}</span>
              <span :class="row.retryCount > 0 ? 'text-cp-orange-text' : undefined">
                重试 {{ formatCompactNumber(row.retryCount) }}
              </span>
            </small>
          </div>
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
