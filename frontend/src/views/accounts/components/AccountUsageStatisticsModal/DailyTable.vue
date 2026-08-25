<script setup lang="ts">
import type { AccountUsageStatisticsDay } from '@/api'
import { computed } from 'vue'

import BaseEmpty from '@/components/base/BaseEmpty.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { formatCompactNumber, formatInteger } from '@/utils/number'
import { formatUsageCost, formatUsagePercent } from '../../utils/accountUsageStatistics'

const props = defineProps<{
  rows: AccountUsageStatisticsDay[]
  isCurrentCycle: boolean
  usedPercent: number | null
}>()
const sectionHeightClass = computed(() => props.rows.length > 4
  ? 'h-[min(15rem,24dvh)]'
  : undefined)

const columns = defineTableColumns<AccountUsageStatisticsDay>([
  { key: 'date', label: '日期', kind: 'custom', size: '2xl' },
  { key: 'creditShare', label: '额度占比', kind: 'numeric', size: 'sm' },
  { key: 'estimatedCost', label: '估算金额', kind: 'numeric', size: 'sm' },
  { key: 'total', label: '总 Tokens', kind: 'numeric', size: 'lg' },
  { key: 'uncachedInput', label: '未缓存输入', kind: 'numeric', size: 'lg' },
  { key: 'cachedInput', label: '缓存输入', kind: 'numeric', size: 'lg' },
  { key: 'output', label: '输出', kind: 'numeric', size: 'md' },
])
</script>

<template>
  <section
    class="flex min-h-0 flex-col overflow-hidden"
    :class="sectionHeightClass"
    aria-labelledby="account-usage-statistics-daily-title"
  >
    <div class="mb-2.5 flex shrink-0 items-center justify-between gap-3">
      <h3
        id="account-usage-statistics-daily-title"
        class="m-0 text-cp-lg font-heavy text-cp-text"
      >
        每日用量
      </h3>
      <span class="text-cp-xs font-semibold text-cp-text-quaternary">官方接口为日粒度</span>
    </div>

    <BaseTable
      v-if="rows.length"
      class="min-h-0 min-w-0 flex-[1_1_auto]"
      :columns="columns"
      :rows="rows"
      row-key="date"
      density="compact"
      scrollbar-always-visible
    >
      <template #date="{ row }">
        <div class="flex min-w-0 items-center gap-1.5">
          <span class="shrink-0 font-mono font-heavy text-cp-text">{{ row.date }}</span>
          <span
            v-if="row.isBoundaryDay"
            class="shrink-0 rounded-full bg-cp-info-bg px-1.5 py-0.5 font-sans text-[10px] font-bold text-cp-info-text"
          >
            跨周期起始日
          </span>
          <span
            v-if="row.hasMissingTokenData"
            class="shrink-0 rounded-full bg-cp-warning-bg px-1.5 py-0.5 font-sans text-[10px] font-bold text-cp-warning-text"
          >
            无 Token 明细
          </span>
        </div>
      </template>
      <template #creditShare="{ row }">
        {{ formatUsagePercent(row.creditShare, true) }}
      </template>
      <template #estimatedCost="{ row }">
        <span class="font-heavy text-cp-success-text">
          {{ row.hasMissingTokenData ? '—' : formatUsageCost(row.estimatedCost, row.hasUnknownPricing) }}
        </span>
      </template>
      <template #total="{ row }">
        <span class="font-heavy text-cp-text" :title="row.hasMissingTokenData ? undefined : formatInteger(row.tokens.total)">
          {{ row.hasMissingTokenData ? '—' : formatCompactNumber(row.tokens.total) }}
        </span>
      </template>
      <template #uncachedInput="{ row }">
        <span :title="row.hasMissingTokenData ? undefined : formatInteger(row.tokens.uncachedInput)">
          {{ row.hasMissingTokenData ? '—' : formatCompactNumber(row.tokens.uncachedInput) }}
        </span>
      </template>
      <template #cachedInput="{ row }">
        <span :title="row.hasMissingTokenData ? undefined : formatInteger(row.tokens.cachedInput)">
          {{ row.hasMissingTokenData ? '—' : formatCompactNumber(row.tokens.cachedInput) }}
        </span>
      </template>
      <template #output="{ row }">
        <span :title="row.hasMissingTokenData ? undefined : formatInteger(row.tokens.output)">
          {{ row.hasMissingTokenData ? '—' : formatCompactNumber(row.tokens.output) }}
        </span>
      </template>
    </BaseTable>

    <BaseEmpty
      v-else
      :title="isCurrentCycle && usedPercent === 0 ? '本周期刚重置' : '该周期没有用量记录'"
      :description="isCurrentCycle && usedPercent === 0 ? '尚未产生任何官方日报数据' : undefined"
      size="sm"
      surface="none"
      class="min-h-0 flex-1"
    />
  </section>
</template>
