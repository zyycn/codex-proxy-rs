<script setup lang="ts">
import type { BackupRecord } from '@/api'

import { defineTableColumns, ZButton, ZCard, ZConfirmDialog, ZIconButton, ZPagination, ZPopover, ZTable } from '@codex-proxy/ui'

import { CircleHelp, Download, Play, RefreshCw, Trash2 } from '@lucide/vue'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

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
  refreshing: boolean
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
}>()

const { t } = useI18n({ useScope: 'global' })

const deleteOpen = defineModel<boolean>('deleteOpen', { required: true })

const columns = computed(() => (defineTableColumns<BackupRecord>([
  {
    key: 'id',
    label: 'ID',
    kind: 'mono',
    size: 'lg',
  },
  {
    key: 'status',
    label: t('common.fields.status'),
    kind: 'status',
  },
  {
    key: 'fileName',
    label: t('settings.backup.fileName'),
    kind: 'text',
  },
  {
    key: 'sizeBytes',
    label: t('settings.backup.size'),
    kind: 'numeric',
  },
  {
    key: 'expiresAt',
    label: t('settings.backup.expirationTime'),
    kind: 'datetime',
  },
  {
    key: 'trigger',
    label: t('settings.backup.triggerMode'),
    kind: 'status',
  },
  {
    key: 'startedAt',
    label: t('settings.backup.startTime'),
    kind: 'datetime',
  },
  {
    key: 'actions',
    label: t('common.fields.actions'),
    kind: 'actions',
  },
])))

function triggerLabel(value: string): string {
  return value === 'manual'
    ? t('settings.backup.manual')
    : t('settings.backup.plan')
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
  return `${size >= 100 || index === 0
    ? Math.round(size)
    : size.toFixed(1)} ${units[index]}`
}

/** 从对象 key 提取文件名（最后一段）。 */
function formatFileName(record: BackupRecord): string {
  if (!record.objectKey)
    return '—'
  return record.objectKey.split('/').pop() || '—'
}

/** 记录 ID 的短标识：`backup_` 前缀 + 前 8 位 hex，与对象 key 文件名一致；完整值通过 title 展示。 */
function shortId(id: string): string {
  const hex = id.startsWith('backup_')
    ? id.slice('backup_'.length)
    : id
  return hex.length > 8
    ? `backup_${hex.slice(0, 8)}`
    : id
}

function canDownload(record: BackupRecord): boolean {
  return record.status === 'completed'
}

function canDelete(record: BackupRecord): boolean {
  return record.status === 'completed' || record.status === 'failed'
}
</script>

<template>
  <ZCard :description="t('settings.backup.exclusions')">
    <template #title>
      <span class="inline-flex items-center gap-1.5">
        <span>{{ t('settings.backup.backupRecords') }}</span>
        <ZPopover trigger="hover-click" placement="bottom-start">
          <template #reference="{ open }">
            <ZIconButton :aria-label="t('settings.backup.backupScopeDescription')" :title="undefined" :aria-expanded="open" class="size-6! hover:bg-transparent! active:bg-transparent!">
              <CircleHelp class="size-3.5" aria-hidden="true" />
            </ZIconButton>
          </template>
          <div class="max-w-72 space-y-2 px-3 py-2 text-cp-sm leading-relaxed text-cp-text-secondary">
            <p>{{ t('settings.backup.contents') }}</p>
            <p>{{ t('settings.backup.usageRetentionHourSummary') }}</p>
          </div>
        </ZPopover>
      </span>
    </template>
    <template #actions>
      <div class="flex flex-wrap items-center gap-2">
        <ZButton
          type="primary"
          :loading="creating"
          :disabled="loading || activeBackup"
          @click="emit('create')"
        >
          <template #icon>
            <Play class="size-4" />
          </template>
          {{ creating ? t('settings.backup.creating') : t('settings.backup.createBackup') }}
        </ZButton>
        <ZButton
          :loading="refreshing"
          :disabled="loading"
          @click="emit('refresh')"
        >
          <template #loading>
            <RefreshCw class="size-4 animate-spin motion-reduce:animate-none" />
          </template>
          <template #icon>
            <RefreshCw class="size-4" />
          </template>
          {{ t('common.actions.refresh') }}
        </ZButton>
      </div>
    </template>

    <div class="flex min-h-0 flex-1 flex-col">
      <ZTable
        class="min-h-0 flex-1"
        :columns="columns"
        :data="records"
        row-key="id"
        :loading="loading"
        :empty-text="t('settings.backup.noBackupRecordYet')"
      >
        <template #id="{ row }">
          <span class="text-cp-text-secondary" :title="row.id">{{ shortId(row.id) }}</span>
        </template>

        <template #status="{ row }">
          <BackupStatusBadge :status="row.status" />
        </template>

        <template #fileName="{ row }">
          <span class="text-cp-text-secondary" :title="row.objectKey">{{ formatFileName(row) }}</span>
        </template>

        <template #sizeBytes="{ row }">
          <span class="text-cp-text-secondary">{{ formatSize(row.sizeBytes) }}</span>
        </template>

        <template #expiresAt="{ row }">
          <span class="text-cp-text-secondary">
            {{ row.expiresAtDisplay ?? '—' }}
          </span>
        </template>

        <template #trigger="{ row }">
          <span class="text-cp-text-secondary">
            {{ triggerLabel(row.triggerKind) }}
          </span>
        </template>

        <template #startedAt="{ row }">
          <span class="text-cp-text-secondary">
            {{ row.startedAtDisplay ?? '—' }}
          </span>
        </template>

        <template #actions="{ row }">
          <div class="flex items-center justify-start gap-1">
            <ZIconButton
              :disabled="!canDownload(row)"
              :loading="Boolean(downloadStates[row.id])"
              :aria-label="t('settings.backup.downloadBackup')"
              @click="emit('download', row)"
            >
              <Download class="size-4" />
            </ZIconButton>
            <ZIconButton
              :disabled="!canDelete(row)"
              :aria-label="t('settings.backup.deleteBackup')"
              @click="emit('requestDelete', row)"
            >
              <Trash2 class="size-4" />
            </ZIconButton>
          </div>
        </template>
      </ZTable>
      <ZPagination
        v-bind="{ currentPage: page, pageSize, total }"
        :disabled="loading"
        @current-change="emit('pageChange', $event)"
        @size-change="emit('pageSizeChange', $event)"
      />
    </div>

    <ZConfirmDialog
      v-model="deleteOpen"
      :title="t('settings.backup.deleteBackup')"
      :description="t('settings.backup.deleteWarning')"
      destructive
      :confirm-text="t('common.actions.delete')"
      :loading="deleting"
      @confirm="emit('confirmDelete')"
    >
      <p class="m-0">
        {{ t('settings.backup.deleteConfirmation') }} <span class="font-emphasis">{{ deleteTarget?.id }}</span> {{ t('settings.backup.message') }}
      </p>
    </ZConfirmDialog>
  </ZCard>
</template>
