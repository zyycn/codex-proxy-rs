<script setup lang="ts">
import type { AccountUsageStatisticsModel } from '@/api'
import { Info } from '@lucide/vue'
import { computed } from 'vue'

import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { formatCompactNumber, formatInteger } from '@/utils/number'
import { formatUsageCost, formatUsagePercent } from '../../utils/accountUsageStatistics'

const props = defineProps<{
  rows: AccountUsageStatisticsModel[]
}>()
const sectionHeightClass = computed(() => props.rows.length > 4
  ? 'h-[min(15rem,24dvh)]'
  : undefined)
const allocationLabel = computed(() => {
  const estimatedRows = props.rows.filter(row => row.hasEstimatedAllocation)
  if (estimatedRows.length === 0)
    return null

  const hasRateFallback = estimatedRows.some(row => row.hasRateFallback)
  const hasRateAllocation = estimatedRows.some(row => !row.hasRateFallback)
  if (hasRateFallback && hasRateAllocation)
    return 'Token 为估算值'

  return hasRateFallback ? 'Token 按额度估算' : 'Token 按费率估算'
})

const columns = defineTableColumns<AccountUsageStatisticsModel>([
  { key: 'model', label: '模型', kind: 'custom', size: 'xl' },
  { key: 'serviceTier', label: '服务档位', kind: 'custom', size: 'sm' },
  { key: 'creditShare', label: '额度占比', kind: 'numeric', size: 'sm' },
  { key: 'quotaShare', label: '占周限额', kind: 'numeric', size: 'sm' },
  { key: 'uncachedInput', label: '未缓存输入', kind: 'numeric', size: 'md' },
  { key: 'cachedInput', label: '缓存输入', kind: 'numeric', size: 'md' },
  { key: 'output', label: '输出', kind: 'numeric', size: 'sm' },
  { key: 'total', label: '总 Tokens', kind: 'numeric', size: 'md' },
  { key: 'estimatedCost', label: '估算金额', kind: 'numeric', size: 'sm' },
])
</script>

<template>
  <section
    class="flex min-h-0 flex-col overflow-hidden"
    :class="sectionHeightClass"
    aria-labelledby="account-usage-statistics-models-title"
  >
    <div class="mb-2.5 flex shrink-0 items-center gap-1.5">
      <h3
        id="account-usage-statistics-models-title"
        class="m-0 shrink-0 text-cp-lg font-heavy text-cp-text"
      >
        模型明细
      </h3>
      <span
        v-if="allocationLabel"
        class="inline-flex shrink-0 cursor-help items-center justify-center text-cp-text-quaternary"
        role="img"
        :aria-label="allocationLabel"
        :title="allocationLabel"
      >
        <Info class="size-3.5" aria-hidden="true" />
      </span>
    </div>

    <BaseTable
      class="min-h-0 min-w-0 flex-[1_1_auto]"
      :columns="columns"
      :rows="rows"
      row-key="key"
      density="compact"
      empty-text="该周期没有模型用量"
      scrollbar-always-visible
    >
      <template #model="{ row }">
        <span class="block truncate font-mono font-heavy text-cp-text">{{ row.model }}</span>
      </template>
      <template #serviceTier="{ row }">
        <span class="rounded-full bg-cp-fill-tertiary px-2 py-1 font-mono text-cp-xs font-bold capitalize">
          {{ row.serviceTier }}
        </span>
      </template>
      <template #creditShare="{ row }">
        {{ formatUsagePercent(row.creditShare, true) }}
      </template>
      <template #quotaShare="{ row }">
        {{ formatUsagePercent(row.quotaShare, true) }}
      </template>
      <template #uncachedInput="{ row }">
        <span :title="formatInteger(row.tokens.uncachedInput)">
          {{ row.hasMissingTokenData && row.tokens.total === 0 ? '无明细' : formatCompactNumber(row.tokens.uncachedInput) }}
        </span>
      </template>
      <template #cachedInput="{ row }">
        <span :title="formatInteger(row.tokens.cachedInput)">{{ formatCompactNumber(row.tokens.cachedInput) }}</span>
      </template>
      <template #output="{ row }">
        <span :title="formatInteger(row.tokens.output)">{{ formatCompactNumber(row.tokens.output) }}</span>
      </template>
      <template #total="{ row }">
        <span class="font-heavy text-cp-text" :title="formatInteger(row.tokens.total)">
          {{ formatCompactNumber(row.tokens.total) }}
        </span>
      </template>
      <template #estimatedCost="{ row }">
        <span class="font-heavy text-cp-success-text">
          {{ formatUsageCost(row.estimatedCost, row.hasUnknownPricing) }}
        </span>
      </template>
    </BaseTable>
  </section>
</template>
