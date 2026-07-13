<script setup lang="ts">
import { CornerDownRight } from '@lucide/vue'
import { computed } from 'vue'

import BaseCard from '@/components/base/BaseCard.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'

import { formatCompactNumber, formatDuration, formatPercent, formatUsd } from '../utils/format'

const props = withDefaults(
  defineProps<{
    diagnostics: any
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

const diagnosticColumns = [
  { key: 'nameDisplay', label: '维度', width: '220px', ellipsis: false },
  {
    key: 'requestCount',
    label: '请求 / 占比',
    width: '104px',
    align: 'right' as const,
    ellipsis: false,
  },
  {
    key: 'errorCount',
    label: '错误 / 错误率',
    width: '114px',
    align: 'right' as const,
    ellipsis: false,
  },
  { key: 'latencyP95Ms', label: 'P95', width: '90px', align: 'right' as const, ellipsis: false },
  {
    key: 'estimatedCost',
    label: '预估费用',
    width: '112px',
    align: 'right' as const,
    ellipsis: false,
    headerClass: '!pr-5',
    cellClass: '!pr-5',
  },
]

const dimensionLabel = computed(
  () => dimensionOptions.find((option) => option.value === dimension.value)?.label ?? '维度',
)

const isStale = computed(() => props.diagnostics.dimension !== dimension.value)

const sortedItems = computed(() =>
  [...props.diagnostics.items].sort(
    (left, right) => right.errorCount - left.errorCount || right.requestCount - left.requestCount,
  ),
)

const displayItems = computed(() =>
  sortedItems.value.map((item) => ({
    ...item,
    nameDisplay: diagnosticNameDisplay(item.name),
  })),
)

const tableRows = computed(() => (isStale.value ? [] : displayItems.value))

function diagnosticNameDisplay(name: string) {
  const full = name.trim() || '未知'
  if (dimension.value !== 'model') {
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
    :padded="false"
    title="热点诊断"
    :description="`按${dimensionLabel}定位错误、慢请求与费用热点`"
    header-collapse-at="lg"
    header-class="px-5 pt-4"
    body-class="min-h-0 min-w-0 px-5 pt-3"
    class="grid h-105 min-h-105 max-h-105 min-w-0 w-full grid-rows-[auto_minmax(0,1fr)] lg:h-90 lg:min-h-90 lg:max-h-90"
  >
    <template #actions>
      <BaseSegmented
        v-model="dimension"
        :options="dimensionOptions"
        :disabled="loading"
        class="w-full min-w-0 lg:w-80"
      />
    </template>

    <template #body>
      <BaseTable
        :key="dimension"
        class="min-h-0 w-full"
        :columns="diagnosticColumns"
        :rows="tableRows"
        :loading="loading || isStale"
        compact
        row-key="name"
        empty-text="暂无诊断数据"
        max-height="230px"
        min-width="640px"
      >
        <template #header-nameDisplay>
          {{ dimensionLabel }}
        </template>

        <template #nameDisplay="{ row }">
          <div class="inline-grid max-w-full min-w-0 gap-1" :title="row.nameDisplay.full">
            <code
              class="block max-w-full truncate font-mono text-[12px] leading-none font-[760] text-(--cp-text-primary)"
            >
              {{ row.nameDisplay.primary }}
            </code>
            <div
              v-if="row.nameDisplay.secondary"
              class="flex min-w-0 items-center gap-1.25 text-(--cp-text-secondary)"
            >
              <CornerDownRight class="size-3.25 shrink-0 text-(--cp-info)" stroke-width="2.4" />
              <code class="block truncate font-mono text-[11px] leading-none font-bold">
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
            <strong class="font-[720] text-(--cp-text-primary)">
              {{ formatCompactNumber(row.requestCount) }}
            </strong>
            <small class="text-[10px] font-[650] text-(--cp-text-muted)">
              {{ formatPercent(row.requestShare) }}
            </small>
          </span>
        </template>

        <template #errorCount="{ row }">
          <span class="grid justify-items-end gap-1 font-mono leading-none tabular-nums">
            <strong
              class="font-[720]"
              :class="row.errorCount > 0 ? 'text-(--cp-danger-text)' : 'text-(--cp-text-primary)'"
            >
              {{ formatCompactNumber(row.errorCount) }}
            </strong>
            <small
              class="text-[10px] font-[650]"
              :class="row.errorRate > 0 ? 'text-(--cp-danger-text)' : 'text-(--cp-text-muted)'"
            >
              {{ formatPercent(row.errorRate) }}
            </small>
          </span>
        </template>

        <template #latencyP95Ms="{ row }">
          <span class="font-mono font-[680] tabular-nums text-(--cp-warning-text)">
            {{ formatDuration(row.latencyP95Ms) }}
          </span>
        </template>

        <template #estimatedCost="{ row }">
          <span class="font-mono font-[680] tabular-nums text-(--cp-success-text)">
            {{ formatUsd(row.estimatedCost) }}
          </span>
        </template>
      </BaseTable>
    </template>
  </BaseCard>
</template>
