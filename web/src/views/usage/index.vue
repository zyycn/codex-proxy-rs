<script setup lang="ts">
import { CalendarDays, Eye } from '@lucide/vue'
import dayjs from 'dayjs'
import { computed, ref, watch } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import {
  statusOptions,
  usageTimeRangeOptions,
  usageAccountText,
  usageReasoningEffort,
  usageRecordColumns,
  usageRecordType,
  usageRecordTypeClass,
} from './constants'
import { useUsageRecordDetail } from './composables/useUsageRecordDetail'
import { useUsageFilters } from './composables/useUsageFilters'
import { useUsageRecordsTable } from './composables/useUsageRecordsTable'
import UsageClientIpCell from './components/UsageClientIpCell.vue'
import UsageCostCell from './components/UsageCostCell.vue'
import UsageFilters from './components/UsageFilters.vue'
import UsageInsightsGrid from './components/UsageInsightsGrid.vue'
import UsageModelCell from './components/UsageModelCell.vue'
import UsageRecordDetailModal from './components/UsageRecordDetailModal.vue'
import UsageSummaryCards from './components/UsageSummaryCards.vue'
import UsageTokenCell from './components/UsageTokenCell.vue'

const totalRecords = ref(0)
const timeRange = ref('7d')
const timeRangeParams = computed<Record<string, string>>(() => {
  const now = dayjs()

  if (timeRange.value === 'today') {
    return {
      startTime: now.startOf('day').toISOString(),
      endTime: now.toISOString(),
    }
  }

  if (timeRange.value === '30d') {
    return {
      startTime: now.subtract(29, 'day').startOf('day').toISOString(),
      endTime: now.toISOString(),
    }
  }

  if (timeRange.value === 'all') {
    return {} as Record<string, string>
  }

  return {
    startTime: now.subtract(6, 'day').startOf('day').toISOString(),
    endTime: now.toISOString(),
  }
})

const {
  page,
  pageSize,
  searchQuery,
  filterStatus,
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
  loadUsageRecords,
  refreshUsageRecords,
} = useUsageRecordsTable({
  page,
  pageSize,
  searchQuery,
  filterStatus,
  timeRangeParams,
  totalRecords,
})

const { showDetailModal, selectedUsageRecord, handleViewDetail } = useUsageRecordDetail()

bindUsageRecordLoader(loadUsageRecords)

watch(timeRange, () => {
  page.value = 1
  void loadUsageRecords('all')
})
</script>

<template>
  <div class="w-full">
    <header class="flex min-h-17 shrink-0 items-start justify-between gap-4">
      <div>
        <h1 class="mt-0 mb-0 text-[34px] leading-[1.15] font-extrabold text-(--cp-text-primary)">
          使用记录
        </h1>
        <p class="mt-2.5 mb-0 text-[15px] leading-[1.15] font-semibold text-(--cp-text-secondary)">
          查看网关请求的模型、端点、Token 与上游响应状态。
        </p>
      </div>
      <div class="flex shrink-0 items-center gap-2">
        <CalendarDays class="size-4 text-(--cp-text-muted)" />
        <BaseSelect v-model="timeRange" :options="usageTimeRangeOptions" class="w-34" />
      </div>
    </header>

    <UsageSummaryCards :summary="summary" />
    <UsageInsightsGrid :insights="insights" :loading="analyticsLoading" />

    <BaseCard
      :padded="false"
      class="mt-5 flex min-h-155 flex-col"
      header-class="px-5 pt-4"
      body-class="flex min-h-[520px] flex-1 px-5 pt-3 pb-4"
    >
      <template #header>
        <div class="grid gap-3">
          <div class="flex flex-wrap items-center justify-between gap-3">
            <div>
              <h2 class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)">
                请求明细
              </h2>
              <p
                class="mt-1.75 mb-0 text-[13px] leading-[1.15] font-[650] text-(--cp-text-secondary)"
              >
                按请求查看网关调用、上游状态与 Token 消耗。
              </p>
            </div>
          </div>

          <UsageFilters
            v-model:status="filterStatus"
            v-model:search="searchQuery"
            :status-options="statusOptions"
            :loading="loading"
            :refreshing="refreshingList"
            @refresh="refreshUsageRecords"
          />
        </div>
      </template>

      <template #body>
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
              class="whitespace-nowrap font-mono text-[12px] leading-none font-[720] text-(--cp-text-primary)"
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
            <span class="whitespace-nowrap text-[12px] font-[720] text-(--cp-text-primary)">
              {{ usageReasoningEffort(row) }}
            </span>
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

          <template #costDetails="{ row }">
            <UsageCostCell :record="row" />
          </template>

          <template #actions="{ row }">
            <div class="flex items-center justify-center">
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
      </template>
    </BaseCard>

    <UsageRecordDetailModal v-model="showDetailModal" :record="selectedUsageRecord" />
  </div>
</template>
