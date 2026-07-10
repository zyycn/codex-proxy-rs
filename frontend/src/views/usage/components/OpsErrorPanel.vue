<script setup lang="ts">
import { Eye, RefreshCw, Search } from '@lucide/vue'
import { shallowRef, toRef } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { opsErrorColumns } from '../constants'
import { useOpsErrorsTable } from '../composables/useOpsErrorsTable'
import OpsErrorDetailModal from './OpsErrorDetailModal.vue'
import UsageStatusCodeBadge from './UsageStatusCodeBadge.vue'

const props = defineProps<{
  timeRangeParams: Record<string, string>
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

const selectedRecord = shallowRef<any | null>(null)
const detailOpen = shallowRef(false)

function showDetail(record: any) {
  selectedRecord.value = record
  detailOpen.value = true
}
</script>

<template>
  <BaseCard
    :padded="false"
    class="mt-5 flex flex-col"
    header-class="px-5 pt-4"
    body-class="flex flex-col px-5 pt-3 pb-4"
  >
    <template #header>
      <div class="grid gap-3">
        <div>
          <h2 class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)">错误明细</h2>
          <p class="mt-1.75 mb-0 text-[13px] leading-[1.15] font-[650] text-(--cp-text-secondary)">
            直接查询失败事实，按请求、端点和失败分类定位链路问题。
          </p>
        </div>

        <div class="flex flex-wrap items-center justify-between gap-3" aria-label="错误明细筛选">
          <div class="flex min-w-0 flex-1 flex-wrap items-center gap-3">
            <BaseInput
              v-model="searchQuery"
              placeholder="搜索消息或精确请求 ID"
              class="min-w-64 flex-1 sm:max-w-96"
            >
              <template #prefix>
                <Search class="size-4.5 text-(--cp-text-tertiary)" />
              </template>
            </BaseInput>
            <BaseInput v-model="failureClass" placeholder="失败分类（精确）" class="w-48" />
            <BaseInput v-model="route" placeholder="端点（精确）" class="w-48" />
          </div>
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
    </template>

    <template #body>
      <BaseTable
        class="min-h-[520px] flex-1"
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
    </template>
  </BaseCard>

  <OpsErrorDetailModal v-model="detailOpen" :record="selectedRecord" />
</template>
