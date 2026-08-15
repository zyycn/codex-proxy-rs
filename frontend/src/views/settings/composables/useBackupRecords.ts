import type { BackupRecord, BackupStatus } from '@/api'

import { computed, onScopeDispose, shallowRef } from 'vue'
import {
  createBackup,
  deleteBackup,
  getBackupDownloadUrl,
  getBackupRecords,
} from '@/api'
import { toast } from '@/components/base/BaseToast'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { errorMessage } from '@/utils/async'

const ACTIVE_STATUSES: BackupStatus[] = ['queued', 'dumping', 'uploading']
const POLL_INTERVAL_MS = 2000

function isActive(status: BackupStatus): boolean {
  return ACTIVE_STATUSES.includes(status)
}

/** 备份记录：分页、可见时轮询、手动创建、下载与请求删除。 */
export function useBackupRecords() {
  const creating = shallowRef(false)
  const refreshing = shallowRef(false)
  const deleting = shallowRef(false)
  const deleteTarget = shallowRef<BackupRecord | null>(null)
  const downloadStates = shallowRef<Record<string, boolean>>({})

  const paged = usePagedQuery<{
    items: BackupRecord[]
    page: { page: number, pageSize: number, total: number, totalPages: number }
  }>({
    initialPageSize: 10,
    load: ({ page, pageSize }) =>
      getBackupRecords({ page, pageSize }),
    onError: () => undefined,
  })

  const records = computed<BackupRecord[]>(() => paged.items.value as BackupRecord[])
  const activeBackup = computed(() =>
    records.value.some(record => isActive(record.status)),
  )

  let timer: number | undefined
  let stopped = false

  function stopPolling(): void {
    stopped = true
    if (timer !== undefined) {
      window.clearTimeout(timer)
      timer = undefined
    }
  }

  function scheduleNextPoll(): void {
    if (stopped)
      return
    if (!activeBackup.value || document.hidden)
      return
    timer = window.setTimeout(async () => {
      timer = undefined
      if (stopped)
        return
      await paged.execute({ silent: true })
      scheduleNextPoll()
    }, POLL_INTERVAL_MS)
  }

  function startPolling(): void {
    stopped = false
    scheduleNextPoll()
  }

  function onVisibilityChange(): void {
    if (document.hidden) {
      if (timer !== undefined) {
        window.clearTimeout(timer)
        timer = undefined
      }
    }
    else {
      scheduleNextPoll()
    }
  }

  async function load(): Promise<void> {
    await paged.execute()
  }

  async function refresh(): Promise<void> {
    if (refreshing.value)
      return
    refreshing.value = true
    try {
      await paged.execute({ silent: true })
    }
    finally {
      refreshing.value = false
    }
  }

  async function changePage(page: number): Promise<void> {
    paged.page.value = page
    await paged.execute()
  }

  async function changePageSize(pageSize: number): Promise<void> {
    paged.pageSize.value = pageSize
    paged.page.value = 1
    await paged.execute()
  }

  async function create(): Promise<void> {
    if (creating.value)
      return
    creating.value = true
    try {
      await createBackup()
      toast.success('备份任务已创建')
      await paged.execute({ silent: true })
    }
    catch (cause) {
      toast.error(errorMessage(cause, '创建备份失败'))
    }
    finally {
      creating.value = false
    }
  }

  async function downloadBackup(record: BackupRecord): Promise<void> {
    if (downloadStates.value[record.id])
      return
    downloadStates.value = { ...downloadStates.value, [record.id]: true }
    try {
      const result = await getBackupDownloadUrl(record.id)
      // 预签名 URL 带 S3 的 Content-Disposition，临时 anchor 触发浏览器下载即可。
      const link = document.createElement('a')
      link.href = result.url
      link.rel = 'noopener'
      document.body.appendChild(link)
      link.click()
      link.remove()
    }
    catch (cause) {
      toast.error(errorMessage(cause, '下载备份失败'))
    }
    finally {
      downloadStates.value = { ...downloadStates.value, [record.id]: false }
    }
  }

  function requestDelete(record: BackupRecord): void {
    deleteTarget.value = record
  }

  async function confirmDelete(): Promise<void> {
    const target = deleteTarget.value
    if (!target || deleting.value)
      return
    deleting.value = true
    try {
      await deleteBackup(target.id)
      toast.success('已请求删除备份')
      await paged.execute({ silent: true })
    }
    catch (cause) {
      toast.error(errorMessage(cause, '删除备份失败'))
    }
    finally {
      deleting.value = false
      deleteTarget.value = null
    }
  }

  document.addEventListener('visibilitychange', onVisibilityChange)
  onScopeDispose(() => {
    document.removeEventListener('visibilitychange', onVisibilityChange)
    stopPolling()
  })

  return {
    records,
    page: paged.page,
    pageSize: paged.pageSize,
    total: paged.total,
    loading: paged.loading,
    error: paged.error,
    activeBackup,
    creating,
    refreshing,
    deleting,
    deleteTarget,
    downloadStates,
    load,
    refresh,
    changePage,
    changePageSize,
    create,
    downloadBackup,
    requestDelete,
    confirmDelete,
    startPolling,
    stopPolling,
  }
}

export type BackupRecordsStore = ReturnType<typeof useBackupRecords>
