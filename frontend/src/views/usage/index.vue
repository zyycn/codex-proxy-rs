<script setup lang="ts">
import type { UsageErrorOrigin, UsageRecordStatus } from '@/api'
import { ZCard, ZDatePicker, ZIconButton, ZPageHeader, ZPagination, ZSelect, ZTableColumnSettings } from '@codex-proxy/ui'
import { Eye } from '@lucide/vue'

import { useElementSize } from '@vueuse/core'
import { computed, shallowRef, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import ProviderFilter from '@/components/ProviderFilter.vue'
import UsageRecordsTable from '@/components/usage/UsageRecordsTable.vue'
import UsageRecordStatusFilter from '@/components/usage/UsageRecordStatusFilter.vue'
import UsageErrorDetailModal from './components/UsageErrorDetailModal.vue'
import UsageFilters from './components/UsageFilters.vue'
import UsageInsightsGrid from './components/UsageInsightsGrid.vue'
import UsageRecordDetailModal from './components/UsageRecordDetailModal.vue'
import UsageSummaryCards from './components/UsageSummaryCards.vue'
import { useUsageRecordColumns } from './composables/useUsageRecordColumns'
import { useUsageRecordDetail } from './composables/useUsageRecordDetail'
import { useUsageRecordsTable } from './composables/useUsageRecordsTable'
import { useUsageTimeRange } from './composables/useUsageTimeRange'
import { isErrorUsageRecord } from './utils/detail'

const { t } = useI18n({ useScope: 'global' })

const recordView = shallowRef<UsageRecordStatus>('success')
const errorOrigin = shallowRef<UsageErrorOrigin | 'all'>('all')
const sourceOptions = computed(() => [
  {
    value: 'all',
    label: t('usage.filters.allSources'),
  },
  ...([
    'upstream',
    'proxy',
    'connection',
    'unknown',
  ] as const)
    .map(value => ({
      value,
      label: t(`usage.error.origin.${value}`),
    })),
])
const { width } = useElementSize(useTemplateRef<HTMLElement>('tableArea'))
const columns = useUsageRecordColumns(recordView, width)
watch(recordView, () => {
  errorOrigin.value = 'all'
}, { flush: 'sync' })

const { timeRange, timeRangeParams, disabledDate, refreshTimeRangeEnd, latestTimeRangeParams }
  = useUsageTimeRange()

const {
  filterField,
  filterValue,
  providerQuery,
  usagePagination,
  loading,
  records,
  summary,
  health,
  performance,
  cost,
  diagnostics,
  performanceMetric,
  costMetric,
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
  errorOrigin,
})

const { showDetailModal, selectedUsageRecord, detailIsError, detailLoading, detailError, handleViewDetail, reloadDetail } = useUsageRecordDetail()
const detailComponent = computed(() => detailIsError.value
  ? UsageErrorDetailModal
  : UsageRecordDetailModal)

watch(timeRange, () => {
  refreshTimeRangeEnd()
  void loadUsageRecords()
})
</script>

<template>
  <div class="@container w-full">
    <ZPageHeader class="justify-end" :title="t('navigation.usage')" :description="t('usage.page.description')">
      <template #actions>
        <div class="flex w-[100cqw] max-w-[100cqw] flex-wrap items-center justify-end gap-2 @min-[21rem]:flex-nowrap @md:w-auto">
          <ZDatePicker
            v-model="timeRange"
            type="daterange"
            :clearable="false"
            :editable="false"
            :disabled-date="disabledDate"
            single-panel
            :aria-label="t('usage.page.dateRangeLabel')"
            class="min-w-0 flex-1 basis-full @min-[21rem]:basis-auto @md:w-60 @md:flex-none @max-md:[&>div]:gap-1.5 @max-md:[&>div]:px-2"
          />
          <ProviderFilter
            v-model="providerQuery"
            :disabled="refreshingList"
            class="shrink-0"
          />
        </div>
      </template>
    </ZPageHeader>

    <UsageSummaryCards :summary="summary.data" :error="summary.error" />
    <UsageInsightsGrid
      v-model:diagnostic-dimension="diagnosticDimension"
      v-model:performance-metric="performanceMetric"
      v-model:cost-metric="costMetric"
      :health="health"
      :performance="performance"
      :cost="cost"
      :diagnostics="diagnostics"
    />

    <ZCard
      class="mt-5 flex flex-col"
    >
      <template #header>
        <div>
          <h2 class="m-0 text-xl leading-[1.15] font-heavy text-z-text">
            {{ t('usage.page.requestDetails') }}
          </h2>
          <p
            class="mt-1.75 mb-0 text-z leading-[1.15] font-emphasis text-z-text-secondary"
          >
            {{ t('usage.page.recordsDescription') }}
          </p>
        </div>
      </template>

      <template #body>
        <div
          class="grid min-h-130 min-w-0 flex-1 grid-rows-[auto_minmax(0,1fr)] gap-3"
        >
          <UsageFilters
            v-model:field="filterField"
            v-model:value="filterValue"
            :loading="loading"
            :refreshing="refreshingList"
            @refresh="refreshUsageRecords"
          >
            <template #actions>
              <ZSelect
                v-if="recordView === 'error'"
                v-model="errorOrigin"
                :options="sourceOptions"
                :aria-label="t('usage.filters.errorSource')"
                class="w-38"
              />
              <UsageRecordStatusFilter v-model="recordView" show-labels />
              <ZTableColumnSettings
                :options="columns.columnOptions.value"
                @change="columns.setColumnVisible"
                @reorder="columns.setColumnOrder"
                @reset="columns.resetColumns"
              />
            </template>
          </UsageFilters>

          <div ref="tableArea" class="flex min-h-0 min-w-0 flex-col">
            <UsageRecordsTable
              class="min-h-0 flex-1"
              :columns="columns.visibleColumns.value"
              request-details
              :rows="records"
              :loading="loading"
              :empty-text="t('usage.page.noRequestsMatchTheseFilters')"
            >
              <template #actions="{ row }">
                <div class="flex items-center justify-start">
                  <ZIconButton
                    size="small"
                    :aria-label="isErrorUsageRecord(row) ? t('usage.page.viewErrorDetails') : t('usage.page.viewUsageRecordDetails')"
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

    <component :is="detailComponent" v-model="showDetailModal" :record="selectedUsageRecord" :loading="detailLoading" :error="detailError" @retry="reloadDetail" />
  </div>
</template>
