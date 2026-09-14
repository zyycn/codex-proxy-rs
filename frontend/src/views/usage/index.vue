<script setup lang="ts">
import { Eye } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'

import BaseIconButton from '@/components/base/BaseIconButton.vue'
import ProviderFilterSegmented from '@/components/business/ProviderFilterSegmented.vue'

import UsageErrorRecords from './components/UsageErrorRecords.vue'
import UsageRecordDetailModal from './components/UsageRecordDetailModal.vue'
import UsageViewContent from './components/UsageViewContent.vue'
import { useUsage } from './composables/useUsage'
import { useUsageRecordDetail } from './composables/useUsageRecordDetail'
import { usageTimeRangeOptions, useUsageTimeRange } from './composables/useUsageTimeRange'

const recordView = shallowRef('success')
const { timeRange, timeRangeParams, refreshTimeRangeEnd, latestTimeRangeParams }
  = useUsageTimeRange()

const {
  isAdmin,
  columns,
  diagnosticDimensionOptions,
  showScheduling,
  searchPlaceholder,
  searchAriaLabel,
  currentPage,
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
  tableError,
  loadUsageRecords,
  refreshUsageRecords,
  handlePageChange,
  handlePageSizeChange,
} = useUsage({
  timeRangeParams,
  latestTimeRangeParams,
  active: computed(() => recordView.value === 'success'),
})

const { showDetailModal, selectedUsageRecord, handleViewDetail } = useUsageRecordDetail()

watch(timeRange, () => {
  refreshTimeRangeEnd()
  currentPage.value = 1
  void loadUsageRecords()
})
</script>

<template>
  <UsageViewContent
    v-model:time-range="timeRange"
    v-model:search="searchQuery"
    v-model:record-view="recordView"
    v-model:diagnostic-dimension="diagnosticDimension"
    :time-range-options="usageTimeRangeOptions"
    :summary="summary"
    :insights="insights"
    :columns="columns"
    :rows="records"
    :pagination="usagePagination"
    :loading="loading"
    :analytics-loading="analyticsLoading"
    :refreshing="refreshingList"
    :table-error="tableError"
    :diagnostic-dimension-options="diagnosticDimensionOptions"
    :show-scheduling="showScheduling"
    :search-placeholder="searchPlaceholder"
    :search-aria-label="searchAriaLabel"
    @refresh="refreshUsageRecords"
    @page-change="handlePageChange"
    @page-size-change="handlePageSizeChange"
  >
    <template v-if="isAdmin" #scope-actions>
      <ProviderFilterSegmented
        v-model="providerQuery"
        :disabled="refreshingList"
        class="w-31 shrink-0"
      />
    </template>

    <template v-if="isAdmin" #record-actions="{ row }">
      <div class="flex items-center justify-start">
        <BaseIconButton
          variant="ghost"
          size="sm"
          label="查看使用记录详情"
          @click="handleViewDetail(row)"
        >
          <Eye class="size-3.5" />
        </BaseIconButton>
      </div>
    </template>

    <template #errors>
      <UsageErrorRecords
        :time-range-params="timeRangeParams"
        :latest-time-range-params="latestTimeRangeParams"
        :provider="providerQuery"
        :active="recordView === 'errors'"
      />
    </template>
  </UsageViewContent>

  <UsageRecordDetailModal
    v-if="isAdmin"
    v-model="showDetailModal"
    :record="selectedUsageRecord"
  />
</template>
