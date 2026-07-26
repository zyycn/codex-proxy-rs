<script setup lang="ts">
import { Eye } from '@lucide/vue'
import { shallowRef, watch } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import ProviderFilterSegmented from '@/components/ProviderFilterSegmented.vue'
import OpsErrorPanel from './components/OpsErrorPanel.vue'
import UsageFilters from './components/UsageFilters.vue'
import UsageInsightsGrid from './components/UsageInsightsGrid.vue'
import UsageRecordDetailModal from './components/UsageRecordDetailModal.vue'
import UsageRecordsTable from './components/UsageRecordsTable.vue'
import UsageSummaryCards from './components/UsageSummaryCards.vue'
import { useUsageRecordDetail } from './composables/useUsageRecordDetail'
import { useUsageRecordsTable } from './composables/useUsageRecordsTable'
import { useUsageTimeRange } from './composables/useUsageTimeRange'
import { usageRecordColumns, usageTimeRangeOptions } from './constants'

const recordView = shallowRef('success')
const recordViewOptions = [
  { label: '成功记录', value: 'success' },
  { label: '错误排查', value: 'errors' },
]
const { timeRange, timeRangeParams, refreshTimeRangeEnd, latestTimeRangeParams }
  = useUsageTimeRange()

const {
  page,
  searchQuery,
  providerQuery,
  usagePagination,
  loading,
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
})

const { showDetailModal, selectedUsageRecord, handleViewDetail } = useUsageRecordDetail()

watch(timeRange, () => {
  refreshTimeRangeEnd()
  page.value = 1
  void loadUsageRecords()
})
</script>

<template>
  <div class="w-full">
    <BasePageHeader title="使用统计" description="查看请求用量、性能趋势与调用错误记录">
      <template #actions>
        <BaseSelect v-model="timeRange" :options="usageTimeRangeOptions" class="w-34" />
        <ProviderFilterSegmented
          v-model="providerQuery"
          :disabled="refreshingList"
          class="w-31 shrink-0"
        />
      </template>
    </BasePageHeader>

    <UsageSummaryCards :summary="summary" />
    <UsageInsightsGrid
      v-model:diagnostic-dimension="diagnosticDimension"
      :overview="insights.overview"
      :diagnostics="insights.diagnostics"
      :loading="analyticsLoading"
    />

    <BaseCard
      :padded="false"
      class="mt-5 flex flex-col"
      header-class="px-5 pt-4"
      body-class="flex min-h-0 flex-col px-5 pt-3 pb-4"
    >
      <template #header>
        <div class="flex flex-wrap items-center justify-between gap-3">
          <div>
            <h2 class="m-0 text-xl leading-[1.15] font-heavy text-(--cp-text-primary)">
              请求明细
            </h2>
            <p
              class="mt-1.75 mb-0 text-[13px] leading-[1.15] font-emphasis text-(--cp-text-secondary)"
            >
              成功请求与失败请求明细
            </p>
          </div>
          <BaseSegmented v-model="recordView" :options="recordViewOptions" class="w-52" />
        </div>
      </template>

      <template #body>
        <div
          v-show="recordView === 'success'"
          class="grid min-h-130 flex-1 grid-rows-[auto_minmax(0,1fr)] gap-3"
        >
          <UsageFilters
            v-model:search="searchQuery"
            :loading="loading"
            :refreshing="refreshingList"
            @refresh="refreshUsageRecords"
          />

          <UsageRecordsTable
            class="min-h-0 flex-1"
            :columns="usageRecordColumns"
            :rows="records"
            :loading="loading"
            :pagination="usagePagination"
            empty-text="暂无使用记录"
            min-width="1920px"
            @page-change="handlePageChange"
            @page-size-change="handlePageSizeChange"
          >
            <template #actions="{ row }">
              <div class="flex items-center justify-start">
                <BaseButton
                  icon-only
                  variant="ghost"
                  size="sm"
                  label="查看使用记录详情"
                  @click="handleViewDetail(row)"
                >
                  <Eye class="size-3.5" />
                </BaseButton>
              </div>
            </template>
          </UsageRecordsTable>
        </div>

        <div v-show="recordView === 'errors'" class="min-h-130 flex-1">
          <OpsErrorPanel :time-range-params="timeRangeParams" />
        </div>
      </template>
    </BaseCard>

    <UsageRecordDetailModal v-model="showDetailModal" :record="selectedUsageRecord" />
  </div>
</template>
