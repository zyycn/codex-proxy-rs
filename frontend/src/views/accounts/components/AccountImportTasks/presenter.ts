import type { AccountImportTask, AccountImportTaskItem, ImportItemStatus } from '@/api'
import dayjs from 'dayjs'

export const itemStates: Record<ImportItemStatus, { label: string, color: string, fill: string }> = {
  pending: { label: '等待中', color: 'bg-cp-fill-alter text-cp-text-secondary', fill: 'bg-cp-fill-secondary' },
  running: { label: '处理中', color: 'bg-cp-primary-container text-cp-primary-on-container', fill: 'bg-cp-primary' },
  succeeded: { label: '已入库', color: 'bg-cp-success-container text-cp-success-on-container', fill: 'bg-cp-success' },
  failed: { label: '失败', color: 'bg-cp-error-container text-cp-error-on-container', fill: 'bg-cp-error' },
  unknown: { label: '待核对', color: 'bg-cp-warning-container text-cp-warning-on-container', fill: 'bg-cp-warning' },
  skipped: { label: '未执行', color: 'bg-cp-fill-tertiary text-cp-text-secondary', fill: 'bg-cp-text-quaternary' },
}

export const outcomeOrder: ImportItemStatus[] = ['succeeded', 'failed', 'unknown', 'running', 'pending', 'skipped']

export function taskLabel(task: AccountImportTask) {
  if (!task.finishedAt)
    return task.stopRequested ? '正在停止' : task.counts.running > 0 ? '导入中' : '排队中'
  if (task.stopRequested)
    return '已停止'
  return task.counts.failed + task.counts.unknown > 0 ? '已结束 · 有待处理项' : '全部完成'
}

export function taskTime(value: string) {
  return dayjs(value).format('MM-DD HH:mm:ss')
}

export function processed(task: AccountImportTask) {
  return task.total - task.counts.pending - task.counts.running
}

export function itemDescription(item: AccountImportTaskItem) {
  if (item.message)
    return item.message.replace(/[。.]\s*$/u, '')
  switch (item.status) {
    case 'succeeded': return `已保存 ${item.accountIds.length} 个账号`
    case 'running': return '正在校验凭据并入库'
    case 'pending': return '等待执行'
    case 'skipped': return '本条未执行'
    case 'failed': return '本条导入未成功'
    case 'unknown': return '请检查账号列表，确认是否已入库'
  }
}
