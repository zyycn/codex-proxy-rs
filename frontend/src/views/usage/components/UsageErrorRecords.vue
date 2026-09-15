<script setup lang="ts">
import type { UsageTimeRangeParams } from '../composables/useUsageTimeRange'
import type { UsageErrorRecord } from '../model/errors'
import { Eye, RefreshCw, Search } from '@lucide/vue'
import { computed, shallowRef, toRef } from 'vue'

import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import ProviderIconGroup from '@/components/business/ProviderIconGroup.vue'

import { useUsageErrors } from '../composables/useUsageErrors'
import { keyOpsErrorColumns, opsErrorColumns } from '../model/columns'
import OpsErrorDetailModal from './OpsErrorDetailModal.vue'

const props = defineProps<{
  timeRangeParams: UsageTimeRangeParams
  latestTimeRangeParams: () => UsageTimeRangeParams
  provider: string
  active: boolean
}>()

const {
  isAdmin,
  loading,
  error,
  refreshing,
  records,
  searchQuery,
  pagination,
  handlePageChange,
  handlePageSizeChange,
  refresh,
} = useUsageErrors({
  timeRangeParams: toRef(props, 'timeRangeParams'),
  latestTimeRangeParams: () => props.latestTimeRangeParams(),
  provider: toRef(props, 'provider'),
  active: toRef(props, 'active'),
})

const columns = computed(() => isAdmin.value ? opsErrorColumns : keyOpsErrorColumns)
const selectedRecord = shallowRef<UsageErrorRecord | null>(null)
const detailOpen = shallowRef(false)
const upstreamSendStateLabels: Record<string, string> = {
  sent: '已发送',
  not_sent: '未发送',
  ambiguous: '状态不明',
}

function showDetail(record: UsageErrorRecord) {
  if (!record.adminRecord)
    return
  selectedRecord.value = record
  detailOpen.value = true
}

function accountText(record: UsageErrorRecord) {
  return record.accountEmail || record.accountName || record.accountId || '未记录'
}

function modelText(record: UsageErrorRecord) {
  return record.requestedModel || record.model || record.upstreamModel || '未记录'
}

function modelTitle(record: UsageErrorRecord) {
  const requested = record.requestedModel
  const upstream = record.upstreamModel
  return requested && upstream && requested !== upstream
    ? `${requested} → ${upstream}`
    : modelText(record)
}

function upstreamSendStateText(value: string | null) {
  return upstreamSendStateLabels[value || ''] ?? value ?? '未记录'
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
          :placeholder="isAdmin ? '请求 ID、密钥名称或账号' : '搜索模型'"
          :aria-label="isAdmin ? '搜索错误：请求 ID、密钥名称或账号' : '按模型搜索错误'"
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
      <p v-if="error && !loading" role="alert" class="text-cp-sm text-cp-error-text">
        {{ error }}。请刷新重试。
      </p>
      <BaseTable
        v-else
        class="min-h-0 flex-1"
        :columns="columns"
        :rows="records"
        :loading="loading"
        empty-text="当前时段没有错误"
      >
        <template #provider="{ row }">
          <ProviderIconGroup
            :provider="row.provider"
            :authentication-kind="row.authenticationKind"
          />
        </template>
        <template #summary="{ row }">
          <div class="min-w-0 py-0.5" :title="row.message || row.summary">
            <div class="flex min-w-0 items-center gap-2">
              <code class="block min-w-0 flex-1 truncate font-mono text-cp-sm font-bold text-cp-error-text">
                {{ row.summary }}
              </code>
              <span
                v-if="row.recoveredAt"
                class="inline-flex h-5 shrink-0 items-center rounded-full bg-cp-success-container px-2 text-cp-xs leading-none font-heavy text-cp-success-on-container"
              >
                已自动恢复
              </span>
            </div>
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
        <template #clientIp="{ row }">
          <span
            class="block max-w-full truncate font-mono text-cp-sm font-bold text-cp-text"
            :title="row.clientIp || '未记录'"
          >
            {{ row.clientIp || '—' }}
          </span>
        </template>
        <template #userAgent="{ row }">
          <span class="block max-w-full wrap-break-word whitespace-normal font-mono text-cp-sm leading-[1.4] font-emphasis text-cp-text-secondary">
            {{ row.userAgent || '—' }}
          </span>
        </template>
        <template #actions="{ row }">
          <BaseIconButton
            v-if="row.adminRecord"
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

  <OpsErrorDetailModal
    v-if="isAdmin"
    v-model="detailOpen"
    :record="selectedRecord?.adminRecord ?? null"
  />
</template>
