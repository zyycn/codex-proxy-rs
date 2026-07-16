<script setup lang="ts">
import { CalendarDays, Eye, Minimize2 } from '@lucide/vue'
import { shallowRef, watch } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import {
  usageTimeRangeOptions,
  usageAccountText,
  usageIsCompact,
  usageRecordColumns,
  usageRecordType,
  usageRecordTypeClass,
} from './constants'
import { useUsageRecordDetail } from './composables/useUsageRecordDetail'
import { useUsageFilters } from './composables/useUsageFilters'
import { useUsageRecordsTable } from './composables/useUsageRecordsTable'
import { useUsageTimeRange } from './composables/useUsageTimeRange'
import UsageClientIpCell from './components/UsageClientIpCell.vue'
import UsageBillingCell from './components/UsageBillingCell.vue'
import UsageFilters from './components/UsageFilters.vue'
import UsageInsightsGrid from './components/UsageInsightsGrid.vue'
import UsageLatencyCell from './components/UsageLatencyCell.vue'
import UsageModelCell from './components/UsageModelCell.vue'
import OpsErrorPanel from './components/OpsErrorPanel.vue'
import UsageRecordDetailModal from './components/UsageRecordDetailModal.vue'
import UsageReasoningEffortCell from './components/UsageReasoningEffortCell.vue'
import UsageSummaryCards from './components/UsageSummaryCards.vue'
import UsageTokenCell from './components/UsageTokenCell.vue'

const totalRecords = shallowRef(0)
const recordView = shallowRef('success')
const recordViewOptions = [
  { label: '成功记录', value: 'success' },
  { label: '错误排查', value: 'errors' },
]
const { timeRange, timeRangeParams, refreshTimeRangeEnd, latestTimeRangeParams } =
  useUsageTimeRange()

const {
  page,
  pageSize,
  searchQuery,
  usagePagination,
  bindUsageRecordLoader,
  handlePageChange,
  handlePageSizeChange,
} = useUsageFilters(totalRecords)

const {
  loading,
  analyticsLoading,
  records,
  summary,
  insights,
  refreshingList,
  diagnosticDimension,
  loadUsageRecords,
  refreshUsageRecords,
} = useUsageRecordsTable({
  page,
  pageSize,
  searchQuery,
  timeRangeParams,
  totalRecords,
  latestTimeRangeParams,
})

const { showDetailModal, selectedUsageRecord, handleViewDetail } = useUsageRecordDetail({
  timeRangeParams,
})

bindUsageRecordLoader(loadUsageRecords)

watch(timeRange, () => {
  refreshTimeRangeEnd()
  page.value = 1
  void loadUsageRecords('all')
})
</script>

<template>
  <div class="w-full">
    <header class="flex min-h-17 shrink-0 items-start justify-between gap-4">
      <div>
        <h1 class="mt-0 mb-0 text-[34px] leading-[1.15] font-extrabold text-(--cp-text-primary)">
          使用统计
        </h1>
        <p class="mt-2.5 mb-0 text-[15px] leading-[1.15] font-semibold text-(--cp-text-secondary)">
          查看请求用量、性能趋势与调用错误记录
        </p>
      </div>
      <div class="flex shrink-0 items-center gap-2">
        <CalendarDays class="size-4 text-(--cp-text-muted)" />
        <BaseSelect v-model="timeRange" :options="usageTimeRangeOptions" class="w-34" />
      </div>
    </header>

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
            <h2 class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)">请求明细</h2>
            <p
              class="mt-1.75 mb-0 text-[13px] leading-[1.15] font-[650] text-(--cp-text-secondary)"
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

          <BaseTable
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
            <template #accountEmail="{ row }">
              <span
                class="block max-w-full truncate font-mono text-[12px] leading-none font-[720] text-(--cp-text-primary)"
                :title="usageAccountText(row)"
              >
                {{ usageAccountText(row) }}
              </span>
            </template>

            <template #clientIp="{ row }">
              <UsageClientIpCell :record="row" />
            </template>

            <template #model="{ row }">
              <UsageModelCell :record="row" />
            </template>

            <template #reasoningEffort="{ row }">
              <UsageReasoningEffortCell :record="row" />
            </template>

            <template #route="{ row }">
              <div class="inline-flex max-w-full items-center gap-1.5 whitespace-nowrap">
                <code class="font-mono text-[12px] font-[650]">{{ row.route || '—' }}</code>
                <span
                  v-if="usageIsCompact(row)"
                  class="inline-flex shrink-0 text-(--cp-warning-text)"
                  title="压缩请求"
                  aria-label="压缩请求"
                >
                  <Minimize2 class="size-3.5" stroke-width="2.4" />
                </span>
              </div>
            </template>

            <template #recordType="{ row }">
              <span
                class="inline-flex h-6 min-w-12 items-center justify-center rounded-full px-2 text-[12px] leading-none font-bold"
                :class="usageRecordTypeClass(row)"
              >
                {{ usageRecordType(row) }}
              </span>
            </template>

            <template #tokenDetails="{ row }">
              <UsageTokenCell :record="row" />
            </template>

            <template #billing="{ row }">
              <UsageBillingCell :record="row" />
            </template>

            <template #latency="{ row }">
              <UsageLatencyCell :record="row" />
            </template>

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
          </BaseTable>
        </div>

        <div v-show="recordView === 'errors'" class="min-h-130 flex-1">
          <OpsErrorPanel :time-range-params="timeRangeParams" />
        </div>
      </template>
    </BaseCard>

    <UsageRecordDetailModal v-model="showDetailModal" :record="selectedUsageRecord" />
  </div>
</template>
