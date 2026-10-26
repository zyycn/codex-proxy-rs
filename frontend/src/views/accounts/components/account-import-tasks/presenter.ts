import type { AccountImportTask, AccountImportTaskItem, ImportItemStatus } from '@/api'
import { computed } from 'vue'
import { t } from '@/i18n'

export const itemStates = computed<Record<ImportItemStatus, { label: string, color: string, fill: string }>>(() => ({
  pending: {
    label: t('common.status.pending'),
    color: 'bg-cp-fill-alter text-cp-text-secondary',
    fill: 'bg-cp-fill-secondary',
  },
  running: {
    label: t('accounts.importTasks.processing'),
    color: 'bg-cp-primary-container text-cp-primary-on-container',
    fill: 'bg-cp-primary',
  },
  succeeded: {
    label: t('accounts.importTasks.alreadyInStock'),
    color: 'bg-cp-success-container text-cp-success-on-container',
    fill: 'bg-cp-success',
  },
  failed: {
    label: t('common.status.failed'),
    color: 'bg-cp-error-container text-cp-error-on-container',
    fill: 'bg-cp-error',
  },
  unknown: {
    label: t('accounts.importTasks.toBeVerified'),
    color: 'bg-cp-warning-container text-cp-warning-on-container',
    fill: 'bg-cp-warning',
  },
  skipped: {
    label: t('accounts.importTasks.notExecuted'),
    color: 'bg-cp-fill-tertiary text-cp-text-secondary',
    fill: 'bg-cp-text-quaternary',
  },
}))

export const outcomeOrder: ImportItemStatus[] = [
  'succeeded',
  'failed',
  'unknown',
  'running',
  'pending',
  'skipped',
]

export function taskLabel(task: AccountImportTask) {
  if (!task.finishedAt) {
    return task.stopRequested
      ? t('accounts.importTasks.stopping')
      : task.counts.running > 0
        ? t('accounts.importTasks.importing')
        : t('accounts.importTasks.queuing')
  }
  if (task.stopRequested)
    return t('accounts.importTasks.stopped')
  return task.counts.failed + task.counts.unknown > 0
    ? t('accounts.importTasks.endedPendingItems')
    : t('accounts.importTasks.allDone')
}

export function processed(task: AccountImportTask) {
  return task.total - task.counts.pending - task.counts.running
}

export function itemDescription(item: AccountImportTaskItem) {
  if (item.message)
    return item.message.replace(/[。.]\s*$/u, '')
  switch (item.status) {
    case 'succeeded': return t('accounts.importTasks.savedAccountSavedAccounts', { count: item.accountIds.length })
    case 'running': return t('accounts.importTasks.verifying')
    case 'pending': return t('accounts.importTasks.waitingForExecution')
    case 'skipped': return t('accounts.importTasks.thisArticleIsNotImplemented')
    case 'failed': return t('accounts.importTasks.entryFailed')
    case 'unknown': return t('accounts.importTasks.checkAccountList')
  }
}
