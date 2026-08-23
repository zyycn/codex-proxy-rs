<script setup lang="ts">
import type { UsageTimeRangeParams } from '../composables/useUsageTimeRange'
import type { getOpsErrors } from '@/api'

import { Eye, RefreshCw, Search } from '@lucide/vue'
import { shallowRef, toRef } from 'vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
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
  <div class="grid min-h-130 min-w-0 w-full flex-1 grid-rows-[auto_minmax(0,1fr)] gap-3">
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
              <Search class="size-4.5 text-cp-text-tertiary" />
            </template>
          </BaseInput>
          <BaseInput v-model="failureClass" placeholder="失败分类（精确）" class="min-w-0" />
          <BaseInput v-model="route" placeholder="端点（精确）" class="min-w-0" />
        </div>
      </div>

      <div class="flex shrink-0 self-end items-center justify-end gap-2 lg:ml-auto">
        <BaseIconButton
          variant="ghost"
          size="md"
          label="刷新错误明细"
          :loading="refreshing"
          :disabled="loading || refreshing"
          @click="refresh"
        >
          <template #loading>
            <RefreshCw class="size-4.5 animate-spin motion-reduce:animate-none" />
          </template>
          <RefreshCw class="size-4.5" />
        </BaseIconButton>
      </div>
    </div>

    <div class="flex min-h-0 min-w-0 flex-col">
      <BaseTable
        class="min-h-0 flex-1"
        :columns="opsErrorColumns"
        :rows="records"
        :loading="loading"
        empty-text="暂无错误明细"
      >
        <template #upstreamStatusCode="{ row }">
          <UsageStatusCodeBadge
            :status-code="typeof row.upstreamStatusCode === 'number' ? row.upstreamStatusCode : null"
          />
        </template>
        <template #clientStatusCode="{ row }">
          <UsageStatusCodeBadge
            :status-code="typeof row.clientStatusCode === 'number' ? row.clientStatusCode : null"
          />
        </template>
        <template #failureClass="{ row }">
          <span class="font-mono text-cp-sm font-bold text-cp-error-text">
            {{ row.failureClass || '—' }}
          </span>
        </template>
        <template #accountId="{ row }">
          <span
            class="block max-w-full truncate font-mono text-cp-sm font-bold text-cp-text"
            :title="row.accountId || ''"
          >
            {{ row.metadata.accountLabel || '—' }}
          </span>
        </template>
        <template #actions="{ row }">
          <BaseIconButton
            variant="ghost"
            size="sm"
            label="查看错误详情"
            @click="showDetail(row)"
          >
            <Eye class="size-3.5" />
          </BaseIconButton>
        </template>
      </BaseTable>
      <BaseTablePagination
        :pagination="pagination"
        :loading="loading"
        @page-change="handlePageChange"
        @page-size-change="handlePageSizeChange"
      />
    </div>
  </div>

  <OpsErrorDetailModal v-model="detailOpen" :record="selectedRecord" />
</template>
