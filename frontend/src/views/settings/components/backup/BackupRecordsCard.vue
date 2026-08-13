<script setup lang="ts">
import type { BackupRecord } from '@/api'

import type { BaseTableColumn } from '@/components/base/BaseTable/columns'

import { Download, Play, RefreshCw, Trash2 } from '@lucide/vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { formatDateTime } from '@/utils/date'

import BackupStatusBadge from './BackupStatusBadge.vue'

defineProps<{
  records: BackupRecord[]
  page: number
  pageSize: number
  total: number
  loading: boolean
  error: string
  activeBackup: boolean
  creating: boolean
  deleting: boolean
  deleteTarget: BackupRecord | null
  downloadStates: Record<string, boolean>
}>()

const emit = defineEmits<{
  pageChange: [page: number]
  pageSizeChange: [pageSize: number]
  create: []
  refresh: []
  download: [record: BackupRecord]
  requestDelete: [record: BackupRecord]
  confirmDelete: []
  cancelDelete: []
}>()

const columns: BaseTableColumn<BackupRecord>[] = [
  { key: 'id', label: 'ID', width: 150 },
  { key: 'status', label: '状态', width: 110 },
  { key: 'fileName', label: '文件名', width: '28%', ellipsis: true },
  { key: 'sizeBytes', label: '大小', width: 100 },
  { key: 'expiresAt', label: '过期时间', width: 170 },
  { key: 'trigger', label: '触发方式', width: 100 },
  { key: 'startedAt', label: '开始时间', width: 170 },
  { key: 'actions', label: '操作', width: 112, minWidth: 112, fixed: 'right' },
]

function triggerLabel(value: string): string {
  return value === 'manual' ? '手动' : '计划'
}

function formatSize(value: unknown): string {
  const bytes = Number(value)
  if (!Number.isFinite(bytes) || bytes <= 0)
    return '—'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let index = 0
  let size = bytes
  while (size >= 1024 && index < units.length - 1) {
    size /= 1024
    index += 1
  }
  return `${size >= 100 || index === 0 ? Math.round(size) : size.toFixed(1)} ${units[index]}`
}

/** 从对象 key 提取文件名（最后一段）。 */
function formatFileName(record: BackupRecord): string {
  if (!record.objectKey)
    return '—'
  return record.objectKey.split('/').pop() || '—'
}

/** 记录 ID 的短标识：`backup_` 前缀 + 前 8 位 hex，与对象 key 文件名一致；完整值通过 title 展示。 */
function shortId(id: string): string {
  const hex = id.startsWith('backup_') ? id.slice('backup_'.length) : id
  return hex.length > 8 ? `backup_${hex.slice(0, 8)}` : id
}

function canDownload(record: BackupRecord): boolean {
  return record.status === 'completed'
}

function canDelete(record: BackupRecord): boolean {
  return record.status === 'completed' || record.status === 'failed'
}
</script>

<template>
  <BaseCard
    title="备份记录"
    description="创建手动备份和管理已有备份记录"
    body-class="mt-3"
  >
    <template #actions>
      <div class="flex flex-wrap items-center gap-2">
        <BaseButton
          variant="primary"
          :loading="creating"
          :disabled="loading || activeBackup"
          @click="emit('create')"
        >
          <template #icon>
            <Play class="size-4" />
          </template>
          {{ creating ? '创建中...' : '创建备份' }}
        </BaseButton>
        <BaseButton variant="default" :disabled="loading" @click="emit('refresh')">
          <template #icon>
            <RefreshCw class="size-4" />
          </template>
          刷新
        </BaseButton>
      </div>
    </template>

    <BaseTable
      class="min-h-0 flex-1"
      :columns="columns"
      :rows="records"
      row-key="id"
      :loading="loading"
      :pagination="{
        page,
        pageSize,
        total,
      }"
      empty-text="暂无备份记录"
      min-width="1180px"
      @page-change="emit('pageChange', $event)"
      @page-size-change="emit('pageSizeChange', $event)"
    >
      <template #id="{ row }">
        <span class="text-(--cp-text-secondary)" :title="row.id">{{ shortId(row.id) }}</span>
      </template>

      <template #status="{ row }">
        <BackupStatusBadge :status="row.status" />
      </template>

      <template #fileName="{ row }">
        <span class="text-(--cp-text-secondary)" :title="row.objectKey">{{ formatFileName(row) }}</span>
      </template>

      <template #sizeBytes="{ row }">
        <span class="text-(--cp-text-secondary)">{{ formatSize(row.sizeBytes) }}</span>
      </template>

      <template #expiresAt="{ row }">
        <span class="text-(--cp-text-secondary)">
          {{ row.expiresAt ? formatDateTime(row.expiresAt) : '—' }}
        </span>
      </template>

      <template #trigger="{ row }">
        <span class="text-(--cp-text-secondary)">
          {{ triggerLabel(row.triggerKind) }}
        </span>
      </template>

      <template #startedAt="{ row }">
        <span class="text-(--cp-text-secondary)">
          {{ row.startedAt ? formatDateTime(row.startedAt) : '—' }}
        </span>
      </template>

      <template #actions="{ row }">
        <div class="flex items-center justify-start gap-1">
          <BaseButton
            variant="ghost"
            icon-only
            :disabled="!canDownload(row)"
            :loading="Boolean(downloadStates[row.id])"
            title="下载备份"
            aria-label="下载备份"
            @click="emit('download', row)"
          >
            <template #icon>
              <Download class="size-4" />
            </template>
          </BaseButton>
          <BaseButton
            variant="ghost"
            icon-only
            :disabled="!canDelete(row)"
            title="删除备份"
            aria-label="删除备份"
            @click="emit('requestDelete', row)"
          >
            <template #icon>
              <Trash2 class="size-4" />
            </template>
          </BaseButton>
        </div>
      </template>
    </BaseTable>

    <BaseConfirmModal
      :model-value="deleteTarget !== null"
      title="删除备份"
      description="将删除远端对象并移除记录，此操作不可撤销"
      variant="danger"
      confirm-text="确认删除"
      :loading="deleting"
      width="480px"
      @update:model-value="emit('cancelDelete')"
      @confirm="emit('confirmDelete')"
    >
      <p class="m-0">
        确定要删除备份 <span class="font-emphasis">{{ deleteTarget?.id }}</span> 吗？
      </p>
    </BaseConfirmModal>
  </BaseCard>
</template>
