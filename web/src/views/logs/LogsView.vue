<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { Search, RefreshCw, Trash2, Eye } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseTable from '@/components/base/BaseTable.vue'
import { withMinimumDuration } from '@/utils/async'

import type { EventLog } from '@/api'
import { clearLogs, getLogDetail, getLogs } from '@/api'
import { toast } from '@/components/base/BaseToast'

const loading = ref(true)
const logs = ref<EventLog[]>([])
const totalLogs = ref(0)
const page = ref(1)
const pageSize = ref(20)
const searchQuery = ref('')
const filterLevel = ref<string>('')
const filterKind = ref<string>('')
const showClearModal = ref(false)
const showDetailModal = ref(false)
const selectedLog = ref<EventLog | null>(null)
const refreshingList = ref(false)
const clearingLogs = ref(false)
let searchTimer: number | undefined

const levelOptions = [
  { label: '全部级别', value: '' },
  { label: '信息', value: 'info' },
  { label: '警告', value: 'warn' },
  { label: '错误', value: 'error' },
]

const kindOptions = [
  { label: '全部类型', value: '' },
  { label: '请求', value: 'request' },
  { label: '响应', value: 'response' },
  { label: '系统', value: 'system' },
]

const logColumns = [
  { key: 'createdAt', label: '时间' },
  { key: 'level', label: '级别' },
  { key: 'kind', label: '类型' },
  { key: 'requestId', label: '请求 ID' },
  { key: 'route', label: '路由' },
  { key: 'statusCode', label: '状态码' },
  { key: 'message', label: '消息' },
  { key: 'actions', label: '操作', width: '76px', align: 'right' as const },
]

const levelColors: Record<string, { bg: string; text: string }> = {
  info: { bg: 'bg-blue-50', text: 'text-blue-700' },
  warn: { bg: 'bg-yellow-50', text: 'text-yellow-700' },
  error: { bg: 'bg-red-50', text: 'text-red-700' },
}

const filteredLogs = computed(() => logs.value)
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
      level: filterLevel.value ? (filterLevel.value as EventLog['level']) : undefined,
      kind: filterKind.value || undefined,
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

async function handleViewDetail(log: EventLog) {
  try {
    const detail = await getLogDetail(log.id)
    selectedLog.value = detail
    showDetailModal.value = true
  } catch (error: any) {
    toast.error(error.message || '加载详情失败')
  }
}

function getLevelLabel(level: string): string {
  const labels: Record<string, string> = {
    info: '信息',
    warn: '警告',
    error: '错误',
  }
  return labels[level] || level
}

onMounted(() => {
  loadLogs()
})

watch([searchQuery, filterLevel, filterKind], () => {
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
    <header class="flex h-17 shrink-0 items-start justify-between">
      <div>
        <h1 class="mt-0 text-[34px] leading-[1.15] font-extrabold mb-0 text-(--cp-text-primary)">
          事件日志
        </h1>
        <p class="mt-2.5 text-[15px] leading-[1.15] font-semibold mb-0 text-(--cp-text-secondary)">
          查看系统运行日志 · 共 {{ totalLogs }} 条
        </p>
      </div>
    </header>

    <div class="mt-6 flex shrink-0 items-center justify-between gap-4">
      <div class="flex items-center gap-3">
        <BaseInput v-model="searchQuery" placeholder="搜索消息、请求 ID 或路由..." class="w-80">
          <template #prefix>
            <Search class="size-4.5 text-(--cp-text-tertiary)" />
          </template>
        </BaseInput>

        <BaseSelect v-model="filterLevel" :options="levelOptions" class="w-36" />

        <BaseSelect v-model="filterKind" :options="kindOptions" class="w-36" />
      </div>

      <div class="flex items-center gap-2">
        <BaseIconButton
          variant="ghost"
          size="md"
          title="刷新列表"
          :loading="refreshingList"
          :disabled="loading"
          @click="refreshLogs"
        >
          <RefreshCw class="size-4.5" />
        </BaseIconButton>

        <BaseButton variant="danger" :disabled="clearingLogs" @click="showClearModal = true">
          <Trash2 class="size-4" />
          清空日志
        </BaseButton>
      </div>
    </div>

    <BaseCard v-loading="loading" class="mt-5 flex min-h-0 flex-1 p-0">
      <BaseTable
        :columns="logColumns"
        :rows="filteredLogs"
        :pagination="logPagination"
        empty-text="暂无日志记录"
        @page-change="handlePageChange"
        @page-size-change="handlePageSizeChange"
      >
        <template #createdAt="{ row }">
          <span class="font-mono text-(--cp-text-secondary)">
            {{ row.createdAtDisplay }}
          </span>
        </template>

        <template #level="{ row }">
          <span
            class="inline-flex items-center rounded-full px-2 py-0.5 text-[12px] font-medium"
            :class="[
              levelColors[row.level]?.bg || 'bg-gray-50',
              levelColors[row.level]?.text || 'text-gray-700',
            ]"
          >
            {{ getLevelLabel(row.level) }}
          </span>
        </template>

        <template #kind="{ row }">
          <span class="capitalize text-(--cp-text-secondary)">
            {{ row.kind }}
          </span>
        </template>

        <template #requestId="{ row }">
          <code class="font-mono text-(--cp-text-primary)">
            {{ row.requestId || '—' }}
          </code>
        </template>

        <template #route="{ row }">
          <code class="font-mono text-(--cp-text-primary)">
            {{ row.route || '—' }}
          </code>
        </template>

        <template #statusCode="{ row }">
          <span
            class="font-mono"
            :class="{
              'text-green-600':
                row.statusCode !== undefined && row.statusCode >= 200 && row.statusCode < 300,
              'text-yellow-600':
                row.statusCode !== undefined && row.statusCode >= 300 && row.statusCode < 400,
              'text-red-600': row.statusCode !== undefined && row.statusCode >= 400,
              'text-(--cp-text-secondary)': row.statusCode === undefined,
            }"
          >
            {{ row.statusCode ?? '—' }}
          </span>
        </template>

        <template #message="{ row }">
          <p class="m-0 max-w-sm truncate text-(--cp-text-primary)">
            {{ row.message }}
          </p>
        </template>

        <template #actions="{ row }">
          <div class="flex items-center justify-end">
            <BaseIconButton
              variant="ghost"
              size="sm"
              title="查看详情"
              @click="handleViewDetail(row)"
            >
              <Eye class="size-3.5" />
            </BaseIconButton>
          </div>
        </template>
      </BaseTable>
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
      <div v-if="selectedLog" class="flex flex-col gap-4">
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
                levelColors[selectedLog.level]?.bg || 'bg-gray-50',
                levelColors[selectedLog.level]?.text || 'text-gray-700',
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
            class="m-0 px-3 py-2.5 rounded-lg bg-(--cp-bg-subtle) text-[13px] text-(--cp-text-primary)"
          >
            {{ selectedLog.message }}
          </p>
        </div>

        <div v-if="selectedLog.metadata">
          <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">元数据</label>
          <pre
            class="m-0 px-3 py-2.5 rounded-lg bg-(--cp-bg-subtle) text-[12px] font-mono text-(--cp-text-primary) overflow-x-auto"
            >{{ JSON.stringify(selectedLog.metadata, null, 2) }}</pre
          >
        </div>
      </div>

      <template #footer>
        <BaseButton variant="primary" @click="showDetailModal = false"> 关闭 </BaseButton>
      </template>
    </BaseModal>
  </div>
</template>
