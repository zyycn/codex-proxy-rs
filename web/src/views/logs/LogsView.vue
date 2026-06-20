<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { Search, RefreshCw, Trash2, Eye } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseSpinner from '@/components/base/BaseSpinner.vue'
import AppTopbar from '@/layout/components/AppTopbar.vue'

import type { EventLog } from '@/api'
import { clearLogs, getLogDetail, getLogs } from '@/api'
import { useToastStore } from '@/stores/modules/toast'

const toast = useToastStore()

const loading = ref(true)
const logs = ref<EventLog[]>([])
const searchQuery = ref('')
const filterLevel = ref<string>('')
const filterKind = ref<string>('')
const showClearModal = ref(false)
const showDetailModal = ref(false)
const selectedLog = ref<EventLog | null>(null)

const levelOptions = [
  { label: '全部级别', value: '' },
  { label: '信息', value: 'info' },
  { label: '警告', value: 'warning' },
  { label: '错误', value: 'error' },
]

const kindOptions = [
  { label: '全部类型', value: '' },
  { label: '请求', value: 'request' },
  { label: '响应', value: 'response' },
  { label: '系统', value: 'system' },
]

const levelColors: Record<string, { bg: string, text: string }> = {
  info: { bg: 'bg-blue-50', text: 'text-blue-700' },
  warning: { bg: 'bg-yellow-50', text: 'text-yellow-700' },
  error: { bg: 'bg-red-50', text: 'text-red-700' },
}

const filteredLogs = computed(() => {
  let result = logs.value

  if (searchQuery.value) {
    const query = searchQuery.value.toLowerCase()
    result = result.filter(log =>
      log.message.toLowerCase().includes(query)
      || log.requestId?.toLowerCase().includes(query)
      || log.route?.toLowerCase().includes(query),
    )
  }

  if (filterLevel.value) {
    result = result.filter(log => log.level === filterLevel.value)
  }

  if (filterKind.value) {
    result = result.filter(log => log.kind === filterKind.value)
  }

  return result
})

async function loadLogs() {
  try {
    loading.value = true
    const data = await getLogs({ limit: 100 })
    logs.value = data
  } catch (error: any) {
    toast.error(error.message || '加载失败')
  } finally {
    loading.value = false
  }
}

async function handleClearLogs() {
  try {
    await clearLogs()
    showClearModal.value = false
    await loadLogs()
    toast.success('日志已清空')
  } catch (error: any) {
    toast.error(error.message || '清空失败')
  }
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

function formatTime(dateStr: string): string {
  const date = new Date(dateStr)
  return date.toLocaleString('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

function getLevelLabel(level: string): string {
  const labels: Record<string, string> = {
    info: '信息',
    warning: '警告',
    error: '错误',
  }
  return labels[level] || level
}

onMounted(() => {
  loadLogs()
})
</script>

<template>
  <div class="w-full min-w-295 p-7">
    <header class="flex h-17 items-start justify-between">
      <div>
        <h1 class="mt-0 text-[34px] leading-[1.15] font-extrabold mb-0 text-(--cp-text-primary)">
          事件日志
        </h1>
        <p class="mt-2.5 text-[15px] leading-[1.15] font-semibold mb-0 text-(--cp-text-secondary)">
          查看系统运行日志 · 共 {{ logs.length }} 条
        </p>
      </div>

      <AppTopbar class="mt-0.5" />
    </header>

    <div class="mt-6 flex items-center justify-between gap-4">
      <div class="flex items-center gap-3">
        <BaseInput
          v-model="searchQuery"
          placeholder="搜索消息、请求 ID 或路由..."
          class="w-80"
        >
          <template #prefix>
            <Search class="size-4.5 text-(--cp-text-tertiary)" />
          </template>
        </BaseInput>

        <BaseSelect
          v-model="filterLevel"
          :options="levelOptions"
          class="w-36"
        />

        <BaseSelect
          v-model="filterKind"
          :options="kindOptions"
          class="w-36"
        />
      </div>

      <div class="flex items-center gap-2">
        <BaseIconButton
          variant="ghost"
          size="md"
          title="刷新列表"
          @click="loadLogs"
        >
          <RefreshCw class="size-4.5" />
        </BaseIconButton>

        <BaseButton
          variant="danger"
          size="md"
          @click="showClearModal = true"
        >
          <Trash2 class="size-4" />
          清空日志
        </BaseButton>
      </div>
    </div>

    <BaseCard class="mt-5 p-0">
      <BaseSpinner v-if="loading" class="py-20" />

      <BaseEmpty
        v-else-if="filteredLogs.length === 0"
        message="暂无日志记录"
        class="py-20"
      />

      <BaseScrollbar v-else max-height="calc(100vh - 280px)">
        <table class="w-full border-separate border-spacing-y-2 text-left">
          <thead>
            <tr class="h-10 text-[11px] font-bold text-(--cp-text-muted)">
              <th class="px-3">时间</th>
              <th class="px-3">级别</th>
              <th class="px-3">类型</th>
              <th class="px-3">请求 ID</th>
              <th class="px-3">路由</th>
              <th class="px-3">状态码</th>
              <th class="px-3">消息</th>
              <th class="px-3 text-right">操作</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="log in filteredLogs"
              :key="log.id"
              class="h-13 transition-colors hover:bg-(--cp-bg-subtle)"
            >
              <td class="px-3 rounded-l-lg">
                <span class="text-[13px] font-mono text-(--cp-text-secondary)">
                  {{ formatTime(log.createdAt) }}
                </span>
              </td>
              <td class="px-3">
                <span
                  class="inline-flex items-center px-2 py-0.5 rounded-full text-[12px] font-medium"
                  :class="[
                    levelColors[log.level]?.bg || 'bg-gray-50',
                    levelColors[log.level]?.text || 'text-gray-700',
                  ]"
                >
                  {{ getLevelLabel(log.level) }}
                </span>
              </td>
              <td class="px-3">
                <span class="text-[13px] text-(--cp-text-secondary) capitalize">
                  {{ log.kind }}
                </span>
              </td>
              <td class="px-3">
                <code class="text-[13px] font-mono text-(--cp-text-primary)">
                  {{ log.requestId || '—' }}
                </code>
              </td>
              <td class="px-3">
                <code class="text-[13px] font-mono text-(--cp-text-primary)">
                  {{ log.route || '—' }}
                </code>
              </td>
              <td class="px-3">
                <span
                  class="text-[13px] font-mono"
                  :class="{
                    'text-green-600': log.statusCode && log.statusCode >= 200 && log.statusCode < 300,
                    'text-yellow-600': log.statusCode && log.statusCode >= 300 && log.statusCode < 400,
                    'text-red-600': log.statusCode && log.statusCode >= 400,
                    'text-(--cp-text-secondary)': !log.statusCode,
                  }"
                >
                  {{ log.statusCode || '—' }}
                </span>
              </td>
              <td class="px-3">
                <p class="m-0 text-[13px] text-(--cp-text-primary) truncate max-w-sm">
                  {{ log.message }}
                </p>
              </td>
              <td class="px-3 rounded-r-lg">
                <div class="flex items-center justify-end">
                  <BaseIconButton
                    variant="ghost"
                    size="sm"
                    title="查看详情"
                    @click="handleViewDetail(log)"
                  >
                    <Eye class="size-3.5" />
                  </BaseIconButton>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
      </BaseScrollbar>
    </BaseCard>

    <!-- 清空日志确认 -->
    <BaseModal
      v-model="showClearModal"
      title="确认清空日志"
      width="480px"
    >
      <p class="text-[14px] text-(--cp-text-secondary)">
        确定要清空所有日志记录吗？此操作不可撤销。
      </p>

      <template #footer>
        <BaseButton
          variant="ghost"
          @click="showClearModal = false"
        >
          取消
        </BaseButton>
        <BaseButton
          variant="danger"
          @click="handleClearLogs"
        >
          确认清空
        </BaseButton>
      </template>
    </BaseModal>

    <!-- 日志详情 -->
    <BaseModal
      v-model="showDetailModal"
      title="日志详情"
      width="720px"
    >
      <div v-if="selectedLog" class="flex flex-col gap-4">
        <div class="grid grid-cols-2 gap-4">
          <div>
            <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">时间</label>
            <p class="m-0 text-[13px] text-(--cp-text-primary)">
              {{ formatTime(selectedLog.createdAt) }}
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
              {{ selectedLog.statusCode || '—' }}
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
              {{ selectedLog.latencyMs ? `${selectedLog.latencyMs}ms` : '—' }}
            </span>
          </div>
        </div>

        <div>
          <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">消息</label>
          <p class="m-0 px-3 py-2.5 rounded-lg bg-(--cp-bg-subtle) text-[13px] text-(--cp-text-primary)">
            {{ selectedLog.message }}
          </p>
        </div>

        <div v-if="selectedLog.metadata">
          <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">元数据</label>
          <pre class="m-0 px-3 py-2.5 rounded-lg bg-(--cp-bg-subtle) text-[12px] font-mono text-(--cp-text-primary) overflow-x-auto">{{ JSON.stringify(selectedLog.metadata, null, 2) }}</pre>
        </div>
      </div>

      <template #footer>
        <BaseButton
          variant="primary"
          @click="showDetailModal = false"
        >
          关闭
        </BaseButton>
      </template>
    </BaseModal>
  </div>
</template>
