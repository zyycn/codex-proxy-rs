<script setup lang="ts">
import type { UsageTimeRangeParams } from '../composables/useUsageTimeRange'
import type { getOpsErrors } from '@/api'

import { Eye, RefreshCw, Search } from '@lucide/vue'
import { shallowRef, toRef } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { useOpsErrorsTable } from '../composables/useOpsErrorsTable'
import { opsErrorColumns } from '../constants'
import OpsErrorDetailModal from './OpsErrorDetailModal.vue'
import UsageStatusCodeBadge from './UsageStatusCodeBadge.vue'

const props = defineProps<{
  timeRangeParams: UsageTimeRangeParams
}>()

const {
  loading,
  refreshing,
  records,
  searchQuery,
  failureClass,
  route,
  pagination,
  handlePageChange,
  handlePageSizeChange,
  refresh,
} = useOpsErrorsTable(toRef(props, 'timeRangeParams'))

const selectedRecord = shallowRef<Awaited<ReturnType<typeof getOpsErrors>>['items'][number] | null>(
  null,
)
const detailOpen = shallowRef(false)

function showDetail(record: Awaited<ReturnType<typeof getOpsErrors>>['items'][number]) {
  selectedRecord.value = record
  detailOpen.value = true
}
</script>

<template>
  <div class="grid min-h-130 flex-1 grid-rows-[auto_minmax(0,1fr)] gap-3">
    <div
      class="flex w-full flex-col gap-3 lg:flex-row lg:flex-wrap lg:items-center"
      role="group"
      aria-label="错误明细筛选与操作"
    >
      <div class="min-w-0 flex-1">
        <div class="grid min-w-0 grid-cols-1 gap-2 sm:grid-cols-2 lg:flex lg:items-center lg:gap-3">
          <BaseInput
            v-model="searchQuery"
            placeholder="搜索消息或精确请求 ID"
            class="min-w-0 sm:col-span-2 lg:min-w-64 lg:flex-1 lg:max-w-96"
          >
            <template #prefix>
              <Search class="size-4.5 text-(--cp-text-tertiary)" />
            </template>
          </BaseInput>
          <BaseInput v-model="failureClass" placeholder="失败分类（精确）" class="min-w-0" />
          <BaseInput v-model="route" placeholder="端点（精确）" class="min-w-0" />
        </div>
      </div>

      <div class="flex shrink-0 self-end items-center justify-end gap-2 lg:ml-auto">
        <BaseButton
          icon-only
          variant="ghost"
          size="md"
          label="刷新错误明细"
          :disabled="loading || refreshing"
          @click="refresh"
        >
          <RefreshCw class="size-4.5" :class="refreshing ? 'animate-spin' : undefined" />
        </BaseButton>
      </div>
    </div>

    <BaseTable
      class="min-h-0 flex-1"
      :columns="opsErrorColumns"
      :rows="records"
      :loading="loading"
      :pagination="pagination"
      empty-text="暂无错误明细"
      min-width="1900px"
      @page-change="handlePageChange"
      @page-size-change="handlePageSizeChange"
    >
      <template #statusCode="{ row }">
        <UsageStatusCodeBadge :status-code="row.statusCode" />
      </template>
      <template #failureClass="{ row }">
        <span class="font-mono text-[12px] font-[680] text-(--cp-danger-text)">
          {{ row.failureClass || '—' }}
        </span>
      </template>
      <template #actions="{ row }">
        <BaseButton
          icon-only
          variant="ghost"
          size="sm"
          label="查看错误详情"
          @click="showDetail(row)"
        >
          <Eye class="size-3.5" />
        </BaseButton>
      </template>
    </BaseTable>
  </div>

  <OpsErrorDetailModal v-model="detailOpen" :record="selectedRecord" />
</template>
