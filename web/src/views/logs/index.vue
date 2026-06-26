<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { Search, RefreshCw, Trash2, Eye } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseTable from '@/components/base/BaseTable.vue'
import { withMinimumDuration } from '@/utils/async'

import { clearLogs, getLogDetail, getLogs } from '@/api'
import { toast } from '@/components/base/BaseToast'

const loading = ref(true)
const logs = ref<any[]>([])
const totalLogs = ref(0)
const page = ref(1)
const pageSize = ref(20)
const searchQuery = ref('')
const filterLevel = ref('')
const showClearModal = ref(false)
const showDetailModal = ref(false)
const selectedLog = ref<any>(null)
const refreshingList = ref(false)
const clearingLogs = ref(false)
const loaded = ref(false)
let searchTimer: number | undefined

const logColumns = [
  {
    key: 'createdAtDisplay',
    label: '时间',
    width: '176px',
    mono: true,
    tabular: true,
    cellClass: 'text-(--cp-text-secondary)',
  },
  { key: 'level', label: '级别', width: '96px', ellipsis: false },
  {
    key: 'kind',
    label: '类型',
    width: '128px',
    cellClass: 'font-[650] text-(--cp-text-secondary)',
  },
  { key: 'requestId', label: '请求 ID', minWidth: '220px', flex: 1.1, mono: true },
  { key: 'route', label: '路由', minWidth: '156px', flex: 0.8, mono: true },
  {
    key: 'statusCode',
    label: '状态',
    width: '92px',
    align: 'center' as const,
    ellipsis: false,
    mono: true,
    tabular: true,
  },
  {
    key: 'latencyMs',
    label: '延迟',
    width: '96px',
    align: 'right' as const,
    ellipsis: false,
    mono: true,
    tabular: true,
    cellClass: 'text-(--cp-text-secondary)',
  },
  { key: 'message', label: '消息', minWidth: '260px', flex: 1.35 },
  {
    key: 'actions',
    label: '操作',
    width: '92px',
    align: 'center' as const,
    ellipsis: false,
    headerClass: '!px-4',
    cellClass: '!px-4',
  },
]

const levelColors: Record<string, { bg: string; text: string }> = {
  info: { bg: 'bg-(--cp-info-bg)', text: 'text-(--cp-info-text)' },
  warn: { bg: 'bg-(--cp-warning-bg)', text: 'text-(--cp-warning-text)' },
  error: { bg: 'bg-(--cp-danger-bg)', text: 'text-(--cp-danger-text)' },
  debug: { bg: 'bg-(--cp-bg-subtle)', text: 'text-(--cp-text-secondary)' },
}

const filteredLogs = computed(() => logs.value)
const initialLoading = computed(() => loading.value && !loaded.value)
const levelOptions = [
  { label: '全部级别', value: '' },
  { label: '信息', value: 'info' },
  { label: '错误', value: 'error' },
]
const logPagination = computed(() => ({
  page: page.value,
  pageSize: pageSize.value,
  total: totalLogs.value,
  pageSizes: [10, 20, 50, 100],
}))

async function loadLogs() {
  try {
    loading.value = true
    const result = await getLogs({
      page: page.value,
      pageSize: pageSize.value,
      level: filterLevel.value || undefined,
      search: searchQuery.value || undefined,
    })
    logs.value = result.items
    pageSize.value = result.page.pageSize ?? pageSize.value
    totalLogs.value = result.page.total ?? result.items.length
    page.value = result.page.page ?? page.value

    if (logs.value.length === 0 && totalLogs.value > 0 && page.value > 1) {
      page.value = Math.max(1, result.page.totalPages ?? page.value - 1)
      await loadLogs()
    }
  } catch (error: any) {
    toast.error(error.message || '加载失败')
  } finally {
    loading.value = false
    loaded.value = true
  }
}

async function refreshLogs() {
  if (refreshingList.value || loading.value) return
  refreshingList.value = true
  try {
    await withMinimumDuration(loadLogs)
  } finally {
    refreshingList.value = false
  }
}

async function handleClearLogs() {
  try {
    clearingLogs.value = true
    await clearLogs()
    showClearModal.value = false
    page.value = 1
    await loadLogs()
    toast.success('日志已清空')
  } catch (error: any) {
    toast.error(error.message || '清空失败')
  } finally {
    clearingLogs.value = false
  }
}

function handlePageChange(nextPage: number) {
  page.value = nextPage
  void loadLogs()
}

function handlePageSizeChange(nextPageSize: number) {
  pageSize.value = nextPageSize
  page.value = 1
  void loadLogs()
}

async function handleViewDetail(log: any) {
  try {
    const detail = await getLogDetail({ id: log.id })
    selectedLog.value = detail
    showDetailModal.value = true
  } catch (error: any) {
    toast.error(error.message || '加载详情失败')
  }
}

function getLevelLabel(level: string): string {
  const labels: Record<string, string> = {
    debug: '调试',
    info: '信息',
    warn: '警告',
    error: '错误',
  }
  return labels[level] || level
}

function statusClass(statusCode?: number) {
  if (statusCode === undefined || statusCode === null) {
    return 'bg-(--cp-bg-subtle) text-(--cp-text-secondary)'
  }

  if (statusCode >= 200 && statusCode < 300) {
    return 'bg-(--cp-success-bg) text-(--cp-success-text)'
  }

  if (statusCode >= 300 && statusCode < 400) {
    return 'bg-(--cp-warning-bg) text-(--cp-warning-text)'
  }

  return 'bg-(--cp-danger-bg) text-(--cp-danger-text)'
}

function latencyText(latencyMs?: number) {
  return latencyMs === undefined || latencyMs === null ? '—' : `${latencyMs} ms`
}

onMounted(() => {
  loadLogs()
})

watch([searchQuery, filterLevel], () => {
  page.value = 1
  if (searchTimer) {
    window.clearTimeout(searchTimer)
  }
  searchTimer = window.setTimeout(() => {
    void loadLogs()
  }, 250)
})

onBeforeUnmount(() => {
  if (searchTimer) {
    window.clearTimeout(searchTimer)
  }
})
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
      v-loading="initialLoading"
      :padded="false"
      class="mt-5 flex min-h-0 flex-1 flex-col"
      header-class="px-5 pt-4"
      body-class="min-h-0 flex-1 px-5 pt-3"
    >
      <template #header>
        <div class="flex flex-wrap items-center justify-between gap-3" aria-label="日志筛选">
          <div class="flex min-w-0 flex-1 flex-wrap items-center gap-3">
            <BaseInput
              v-model="searchQuery"
              placeholder="搜索消息、请求 ID 或路由"
              class="min-w-64 flex-1 sm:max-w-96"
            >
              <template #prefix>
                <Search class="size-4.5 text-(--cp-text-tertiary)" />
              </template>
            </BaseInput>

            <BaseSelect v-model="filterLevel" :options="levelOptions" class="w-34" />
          </div>

          <div class="flex shrink-0 items-center gap-2">
            <BaseButton
              icon-only
              variant="ghost"
              size="md"
              label="刷新日志"
              :loading="refreshingList"
              :disabled="loading"
              @click="refreshLogs"
            >
              <RefreshCw class="size-4.5" />
            </BaseButton>

            <BaseButton variant="danger" :disabled="clearingLogs" @click="showClearModal = true">
              <template #icon>
                <Trash2 class="size-4" />
              </template>
              清空日志
            </BaseButton>
          </div>
        </div>
      </template>

      <template #body>
        <BaseTable
          :columns="logColumns"
          :rows="filteredLogs"
          :loading="loading"
          :pagination="logPagination"
          empty-text="暂无日志记录"
          min-width="1280px"
          @page-change="handlePageChange"
          @page-size-change="handlePageSizeChange"
        >
          <template #level="{ row }">
            <span
              class="inline-flex h-6 min-w-12 items-center justify-center rounded-full px-2 text-[12px] leading-none font-bold"
              :class="[
                levelColors[row.level]?.bg || 'bg-(--cp-bg-subtle)',
                levelColors[row.level]?.text || 'text-(--cp-text-secondary)',
              ]"
            >
              {{ getLevelLabel(row.level) }}
            </span>
          </template>

          <template #statusCode="{ row }">
            <span
              class="inline-flex h-6 min-w-12 items-center justify-center rounded-full px-2 leading-none font-bold"
              :class="statusClass(row.statusCode)"
            >
              {{ row.statusCode ?? '—' }}
            </span>
          </template>

          <template #latencyMs="{ row }">
            {{ latencyText(row.latencyMs) }}
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
      message="确定要清空所有日志记录吗？此操作不可撤销。"
      variant="danger"
      confirm-text="确认清空"
      :loading="clearingLogs"
      width="480px"
      @confirm="handleClearLogs"
    />

    <!-- 日志详情 -->
    <BaseModal
      v-model="showDetailModal"
      title="日志详情"
      description="查看单条事件的请求、状态和元数据。"
      variant="info"
      width="720px"
    >
      <div v-if="selectedLog" class="flex max-h-[min(70vh,760px)] flex-col gap-4 overflow-hidden">
        <div class="grid grid-cols-2 gap-4">
          <div>
            <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">时间</label>
            <p class="m-0 text-[13px] text-(--cp-text-primary)">
              {{ selectedLog.createdAtDisplay }}
            </p>
          </div>
          <div>
            <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">级别</label>
            <span
              class="inline-flex items-center px-2 py-0.5 rounded-full text-[12px] font-medium"
              :class="[
                levelColors[selectedLog.level]?.bg || 'bg-(--cp-bg-subtle)',
                levelColors[selectedLog.level]?.text || 'text-(--cp-text-secondary)',
              ]"
            >
              {{ getLevelLabel(selectedLog.level) }}
            </span>
          </div>
          <div>
            <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">请求 ID</label>
            <code class="text-[13px] font-mono text-(--cp-text-primary)">
              {{ selectedLog.requestId || '—' }}
            </code>
          </div>
          <div>
            <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">状态码</label>
            <span class="text-[13px] font-mono text-(--cp-text-primary)">
              {{ selectedLog.statusCode ?? '—' }}
            </span>
          </div>
          <div>
            <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">路由</label>
            <code class="text-[13px] font-mono text-(--cp-text-primary)">
              {{ selectedLog.route || '—' }}
            </code>
          </div>
          <div>
            <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">延迟</label>
            <span class="text-[13px] text-(--cp-text-primary)">
              {{ selectedLog.latencyMs !== undefined ? `${selectedLog.latencyMs}ms` : '—' }}
            </span>
          </div>
        </div>

        <div>
          <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">消息</label>
          <p
            class="m-0 rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 py-2.5 text-[13px] text-(--cp-text-primary)"
          >
            {{ selectedLog.message }}
          </p>
        </div>

        <div v-if="selectedLog.metadata" class="flex min-h-0 flex-1 flex-col">
          <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">元数据</label>
          <BaseScrollbar
            max-height="min(42vh, 420px)"
            view-class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 py-2.5"
          >
            <pre
              class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-[12px] leading-[1.65] text-(--cp-text-primary)"
              >{{ JSON.stringify(selectedLog.metadata, null, 2) }}</pre
            >
          </BaseScrollbar>
        </div>
      </div>

      <template #footer>
        <BaseButton variant="primary" @click="showDetailModal = false"> 关闭 </BaseButton>
      </template>
    </BaseModal>
  </div>
</template>
