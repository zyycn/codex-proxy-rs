<script setup lang="ts">
import type { UsageRecordStatus } from '@/api'
import { useTableColumns, ZCard, ZIconButton, ZPageHeader, ZPagination, ZSegmented, ZSelect, ZTableColumnSettings } from '@codex-proxy/ui'

import { CloudRain, Eye, List, Sun } from '@lucide/vue'
import { shallowRef, watch } from 'vue'
import ProviderFilter from '@/components/ProviderFilter.vue'
import { usageRecordColumns } from '@/components/usage/shared/columns'
import UsageRecordsTable from '@/components/usage/UsageRecordsTable.vue'
import UsageFilters from './components/UsageFilters.vue'
import UsageInsightsGrid from './components/UsageInsightsGrid.vue'
import UsageRecordDetailModal from './components/UsageRecordDetailModal.vue'
import UsageSummaryCards from './components/UsageSummaryCards.vue'
import { useUsageRecordDetail } from './composables/useUsageRecordDetail'
import { useUsageRecordsTable } from './composables/useUsageRecordsTable'
import { useUsageTimeRange } from './composables/useUsageTimeRange'
import { usageTimeRangeOptions } from './constants'

const recordView = shallowRef<UsageRecordStatus>('success')
const { visibleColumns, columnOptions, setColumnVisible, setColumnOrder, resetColumns } = useTableColumns(usageRecordColumns, 'usage-records')
const recordViewOptions = [
  { label: '成功', value: 'success', icon: Sun },
  { label: '错误', value: 'error', icon: CloudRain },
  { label: '全部', value: 'all', icon: List },
]
const { timeRange, timeRangeParams, refreshTimeRangeEnd, latestTimeRangeParams }
  = useUsageTimeRange()

const {
  currentPage,
  searchQuery,
  providerQuery,
  usagePagination,
  loading,
  error,
  analyticsLoading,
  records,
  summary,
  insights,
  refreshingList,
  diagnosticDimension,
  loadUsageRecords,
  refreshUsageRecords,
  handlePageChange,
  handlePageSizeChange,
} = useUsageRecordsTable({
  timeRangeParams,
  latestTimeRangeParams,
  status: recordView,
})

const { showDetailModal, selectedUsageRecord, handleViewDetail } = useUsageRecordDetail()

watch(timeRange, () => {
  refreshTimeRangeEnd()
  currentPage.value = 1
  void loadUsageRecords()
})
</script>

<template>
  <div class="w-full">
    <ZPageHeader title="使用统计" description="查看请求用量、性能趋势与调用错误记录">
      <template #actions>
        <ZSelect v-model="timeRange" :options="usageTimeRangeOptions" class="w-34" />
        <ProviderFilter
          v-model="providerQuery"
          :disabled="refreshingList"
          class="shrink-0"
        />
      </template>
    </ZPageHeader>

    <UsageSummaryCards :summary="summary" />
    <UsageInsightsGrid
      v-model:diagnostic-dimension="diagnosticDimension"
      :overview="insights.overview"
      :diagnostics="insights.diagnostics"
      :loading="analyticsLoading"
    />

    <ZCard
      class="mt-5 flex flex-col"
    >
      <template #header>
        <div>
          <h2 class="m-0 text-xl leading-[1.15] font-heavy text-cp-text">
            请求明细
          </h2>
          <p
            class="mt-1.75 mb-0 text-cp leading-[1.15] font-emphasis text-cp-text-secondary"
          >
            成功请求与失败请求明细
          </p>
        </div>
      </template>

      <template #body>
        <div
          class="grid min-h-130 min-w-0 flex-1 grid-rows-[auto_minmax(0,1fr)] gap-3"
        >
          <UsageFilters
            v-model:search="searchQuery"
            :loading="loading"
            :refreshing="refreshingList"
            @refresh="refreshUsageRecords"
          >
            <template #actions>
              <ZSegmented v-model="recordView" aria-label="请求明细类型" :options="recordViewOptions" display="icon" class="w-30 shrink-0" />
              <ZTableColumnSettings
                :options="columnOptions"
                @change="setColumnVisible"
                @reorder="setColumnOrder"
                @reset="resetColumns"
              />
            </template>
          </UsageFilters>

          <div class="flex min-h-0 min-w-0 flex-col">
            <UsageRecordsTable
              class="min-h-0 flex-1"
              :columns="visibleColumns"
              :rows="records"
              :loading="loading"
              :empty-text="error ? `加载失败：${error}` : '所选条件下暂无请求记录'"
            >
              <template #actions="{ row }">
                <div class="flex items-center justify-start">
                  <ZIconButton
                    size="small"
                    aria-label="查看使用记录详情"
                    @click="handleViewDetail(row)"
                  >
                    <Eye class="size-3.5" />
                  </ZIconButton>
                </div>
              </template>
            </UsageRecordsTable>
            <ZPagination
              v-bind="usagePagination"
              :disabled="loading"
              @current-change="handlePageChange"
              @size-change="handlePageSizeChange"
            />
          </div>
        </div>
      </template>
    </ZCard>

    <UsageRecordDetailModal v-model="showDetailModal" :record="selectedUsageRecord" />
  </div>
</template>
