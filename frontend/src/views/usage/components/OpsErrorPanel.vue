<script setup lang="ts">
import type { UsageTimeRangeParams } from '../composables/useUsageTimeRange'
import type { OpsError } from '@/api'

import { Eye, RefreshCw, Search } from '@lucide/vue'
import { shallowRef, toRef } from 'vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { useOpsErrorsTable } from '../composables/useOpsErrorsTable'
import { opsErrorColumns } from '../constants'
import OpsErrorDetailModal from './OpsErrorDetailModal.vue'

const props = defineProps<{
  timeRangeParams: UsageTimeRangeParams
}>()

const {
  loading,
  refreshing,
  records,
  searchQuery,
  pagination,
  handlePageChange,
  handlePageSizeChange,
  refresh,
} = useOpsErrorsTable(toRef(props, 'timeRangeParams'))

const selectedRecord = shallowRef<OpsError | null>(null)
const detailOpen = shallowRef(false)

const upstreamSendStateLabels: Record<string, string> = {
  sent: '已发送',
  not_sent: '未发送',
  ambiguous: '状态不明',
}

function showDetail(record: OpsError) {
  selectedRecord.value = record
  detailOpen.value = true
}

function accountText(record: OpsError) {
  return record.accountEmail
    || record.accountName
    || record.metadata.accountLabel
    || record.accountId
    || '未记录'
}

function modelText(record: OpsError) {
  return record.requestedModel || record.model || record.upstreamModel || '未记录'
}

function modelTitle(record: OpsError) {
  const requested = record.requestedModel
  const upstream = record.upstreamModel
  return requested && upstream && requested !== upstream
    ? `${requested} → ${upstream}`
    : modelText(record)
}

function upstreamSendStateText(value: string | null | undefined) {
  if (!value)
    return '未记录'
  return upstreamSendStateLabels[value] ?? value
}
</script>

<template>
  <div class="grid min-h-130 min-w-0 w-full flex-1 grid-rows-[auto_minmax(0,1fr)] gap-3">
    <div
      class="flex w-full flex-col gap-3 lg:flex-row lg:flex-wrap lg:items-center"
      role="group"
      aria-label="错误筛选与操作"
    >
      <div class="min-w-0 flex-1">
        <BaseInput
          v-model="searchQuery"
          placeholder="请求 ID、Key 或账号"
          class="min-w-0 w-full lg:max-w-96"
        >
          <template #prefix>
            <Search class="size-4.5 text-cp-text-tertiary" />
          </template>
        </BaseInput>
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
        empty-text="当前时段没有错误"
      >
        <template #provider="{ row }">
          <ProviderIconGroup
            :provider="String(row.provider || '')"
            :authentication-kind="row.authenticationKind"
          />
        </template>
        <template #message="{ row }">
          <div class="min-w-0 py-0.5" :title="row.message || row.providerErrorCode || row.failureClass">
            <code class="block max-w-full truncate font-mono text-cp-sm font-bold text-cp-error-text">
              {{ row.providerErrorCode || row.failureClass }}
            </code>
            <p
              v-if="row.message"
              class="mt-1 mb-0 line-clamp-1 text-cp-xs leading-[1.45] font-emphasis text-cp-text-secondary"
            >
              {{ row.message }}
            </p>
          </div>
        </template>
        <template #upstreamSendState="{ row }">
          <span
            class="inline-flex h-6 max-w-full items-center rounded-full bg-cp-fill-quaternary px-2.5 font-mono text-cp-sm leading-none font-bold text-cp-text-secondary"
            :title="row.upstreamSendState || '未记录'"
          >
            <span class="min-w-0 truncate">{{ upstreamSendStateText(row.upstreamSendState) }}</span>
          </span>
        </template>
        <template #accountId="{ row }">
          <span
            class="block max-w-full truncate font-mono text-cp-sm font-bold text-cp-text"
            :title="accountText(row)"
          >
            {{ accountText(row) }}
          </span>
        </template>
        <template #model="{ row }">
          <span
            class="block max-w-full truncate font-mono text-cp-sm font-bold text-cp-text"
            :title="modelTitle(row)"
          >
            {{ modelText(row) }}
          </span>
        </template>
        <template #actions="{ row }">
          <BaseIconButton
            variant="ghost"
            size="md"
            label="查看错误详情"
            @click="showDetail(row)"
          >
            <Eye class="size-4.5" />
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
