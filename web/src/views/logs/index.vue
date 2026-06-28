<script setup lang="ts">
import { Eye } from '@lucide/vue'
import { ref } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { levelOptions, logColumns } from './constants'
import { useLogDetail } from './composables/useLogDetail'
import { useLogFilters } from './composables/useLogFilters'
import { useLogsTable } from './composables/useLogsTable'
import LogDetailModal from './components/LogDetailModal.vue'
import LogFilters from './components/LogFilters.vue'
import LogLevelBadge from './components/LogLevelBadge.vue'
import LogStatusCodeBadge from './components/LogStatusCodeBadge.vue'

const totalLogs = ref(0)

const {
  page,
  pageSize,
  searchQuery,
  filterLevel,
  logPagination,
  bindLogLoader,
  handlePageChange,
  handlePageSizeChange,
} = useLogFilters(totalLogs)

const {
  loading,
  logs,
  showClearModal,
  refreshingList,
  clearingLogs,
  loadLogs,
  refreshLogs,
  handleClearLogs,
} = useLogsTable({
  page,
  pageSize,
  searchQuery,
  filterLevel,
  totalLogs,
})

const { showDetailModal, selectedLog, handleViewDetail } = useLogDetail()

bindLogLoader(loadLogs)
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col overflow-hidden">
    <header class="flex min-h-17 shrink-0 items-start justify-between gap-4">
      <div>
        <h1 class="mt-0 mb-0 text-[34px] leading-[1.15] font-extrabold text-(--cp-text-primary)">
          事件日志
        </h1>
        <p class="mt-2.5 mb-0 text-[15px] leading-[1.15] font-semibold text-(--cp-text-secondary)">
          追踪网关请求、上游响应与异常线索。
        </p>
      </div>
    </header>

    <BaseCard
      :padded="false"
      class="mt-5 flex min-h-0 flex-1 flex-col"
      header-class="px-5 pt-4"
      body-class="min-h-0 flex-1 px-5 pt-3"
    >
      <template #header>
        <LogFilters
          v-model:level="filterLevel"
          v-model:search="searchQuery"
          :clearing="clearingLogs"
          :level-options="levelOptions"
          :loading="loading"
          :refreshing="refreshingList"
          @clear="showClearModal = true"
          @refresh="refreshLogs"
        />
      </template>

      <template #body>
        <BaseTable
          :columns="logColumns"
          :rows="logs"
          :loading="loading"
          :pagination="logPagination"
          empty-text="暂无日志记录"
          min-width="1280px"
          @page-change="handlePageChange"
          @page-size-change="handlePageSizeChange"
        >
          <template #level="{ row }">
            <LogLevelBadge :level="row.level" compact />
          </template>

          <template #statusCode="{ row }">
            <LogStatusCodeBadge :status-code="row.statusCode" />
          </template>

          <template #actions="{ row }">
            <div class="flex items-center justify-center">
              <BaseButton
                icon-only
                variant="ghost"
                size="sm"
                label="查看日志详情"
                @click="handleViewDetail(row)"
              >
                <Eye class="size-3.5" />
              </BaseButton>
            </div>
          </template>
        </BaseTable>
      </template>
    </BaseCard>

    <BaseConfirmModal
      v-model="showClearModal"
      title="确认清空日志"
      description="清空后无法恢复，新的代理事件会继续记录。"
      variant="danger"
      confirm-text="确认清空"
      :loading="clearingLogs"
      width="480px"
      @confirm="handleClearLogs"
    >
      <p class="m-0">确定要清空所有日志记录吗？此操作不可撤销。</p>
    </BaseConfirmModal>

    <LogDetailModal v-model="showDetailModal" :log="selectedLog" />
  </div>
</template>
