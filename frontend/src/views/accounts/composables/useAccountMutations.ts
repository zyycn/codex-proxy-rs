import type { Ref } from 'vue'
import type {
  getAccounts,
} from '@/api'

import type { BaseTableSort } from '@/components/base/BaseTable/columns'
import dayjs from 'dayjs'
import { ref } from 'vue'
import {
  deleteAccounts,
  exportAccounts,
  getAccountQuota,
  refreshAccount,
  updateAccount,
} from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useDownload } from '@/composables/useDownload'
import { useIdSet } from '@/composables/useIdSet'
import { errorMessage, withMinimumDuration } from '@/utils/async'

import { useAccountOnboarding } from './useAccountOnboarding'

type AccountListResult = Awaited<ReturnType<typeof getAccounts>>
type AccountRow = AccountListResult['items'][number]

const attentionAccountStatuses = new Set(['expired', 'disabled', 'banned'])
const accountStatusSortRank: Record<string, number> = {
  active: 0,
  quota_exhausted: 1,
  expired: 2,
  disabled: 3,
  banned: 4,
}

export function useAccountMutations(options: {
  accounts: Ref<AccountRow[]>
  accountSummary: Ref<AccountListResult['summary']>
  statusQuery: Ref<string>
  sort: Ref<BaseTableSort | undefined>
  selectedIds: Ref<Set<string>>
  totalAccounts: Ref<number>
  reload: () => Promise<unknown>
}) {
  const { downloadJson } = useDownload()
  const accounts = options.accounts
  const accountSummary = options.accountSummary
  const loadAccounts = options.reload
  const onboarding = useAccountOnboarding({ reload: loadAccounts })
  const showDeleteModal = ref(false)
  const showSingleDeleteModal = ref(false)
  const pendingDeleteAccount = ref<AccountRow | null>(null)
  const refreshingAccounts = useIdSet<string>()
  const refreshingQuotaAccounts = useIdSet<string>()
  const updatingStatusAccounts = useIdSet<string>()
  const deletingAccountAction = useAsyncAction()
  const batchDeletingAction = useAsyncAction()
  const exportingAccountsAction = useAsyncAction()
  const refreshingAccountIds = refreshingAccounts.ids
  const refreshingQuotaAccountIds = refreshingQuotaAccounts.ids
  const updatingStatusAccountIds = updatingStatusAccounts.ids
  const deletingAccount = deletingAccountAction.loading
  const batchDeleting = batchDeletingAction.loading
  const exportingAccounts = exportingAccountsAction.loading

  function requestDeleteAccount(account: AccountRow) {
    pendingDeleteAccount.value = account
    showSingleDeleteModal.value = true
  }

  async function handleDelete() {
    if (deletingAccount.value)
      return

    const accountId = pendingDeleteAccount.value?.id
    if (!accountId)
      return

    await deletingAccountAction.run(
      async () => {
        await deleteAccounts({ ids: [accountId] })
        showSingleDeleteModal.value = false
        pendingDeleteAccount.value = null
        await loadAccounts()
        toast.success('账号已删除')
      },
      { errorText: '删除失败' },
    )
  }

  async function handleBatchDelete() {
    if (batchDeleting.value)
      return
    if (options.selectedIds.value.size === 0)
      return

    await batchDeletingAction.run(
      async () => {
        await deleteAccounts({ ids: [...options.selectedIds.value] })
        options.selectedIds.value = new Set()
        showDeleteModal.value = false
        await loadAccounts()
      },
      {
        errorText: false,
        onError: (error) => {
          console.error('Failed to batch delete accounts:', error)
        },
      },
    )
  }

  async function handleExportAccounts() {
    if (exportingAccounts.value)
      return
    const selected = [...options.selectedIds.value]
    if (selected.length === 0) {
      toast.warning('请选择要导出的账号')
      return
    }

    await exportingAccountsAction.run(
      async () => {
        const payload = await exportAccounts({
          ids: selected.join(','),
          confirm: 'export_sensitive_accounts',
        })
        await downloadJson(payload, exportFileName(selected.length))
        toast.success(`已导出 ${selected.length} 个账号`)
      },
      { errorText: '导出失败' },
    )
  }

  async function handleRefresh(accountId: string) {
    await refreshingAccounts.run(accountId, async () => {
      try {
        const result = await withMinimumDuration(async () => {
          const result = await refreshAccount({ id: accountId })
          await loadAccounts()
          return result
        })
        if (result.result === 'alive') {
          toast.success('Token 已刷新')
        }
        else if (result.result === 'skipped') {
          toast.warning(result.error || 'Token 正在刷新中')
        }
        else {
          toast.error(result.error || '刷新失败')
        }
      }
      catch (error: unknown) {
        toast.error(errorMessage(error, '刷新失败'))
      }
    })
  }

  async function handleRefreshQuota(accountId: string) {
    await refreshingQuotaAccounts.run(accountId, async () => {
      try {
        await withMinimumDuration(async () => {
          const result = await getAccountQuota({ id: accountId })
          mergeAccountQuotaRefresh(accountId, result)
        })
      }
      catch (error) {
        console.error('Failed to refresh account quota:', error)
      }
    })
  }

  function mergeAccountQuotaRefresh(
    accountId: string,
    result: Awaited<ReturnType<typeof getAccountQuota>>,
  ) {
    accounts.value = accounts.value.map(account =>
      account.id === accountId ? { ...account, ...result.account } : account,
    )
  }

  function patchAccountStatus(accountId: string, status: string) {
    const current = accounts.value.find(account => account.id === accountId)
    if (!current)
      return

    if (options.statusQuery.value && options.statusQuery.value !== status) {
      accounts.value = accounts.value.filter(account => account.id !== accountId)
      options.totalAccounts.value = Math.max(0, options.totalAccounts.value - 1)
      options.selectedIds.value = new Set(
        [...options.selectedIds.value].filter(id => id !== accountId),
      )
    }
    else {
      const nextAccounts = accounts.value.map(account =>
        account.id === accountId
          ? {
              ...account,
              status,
              displayStatus: account.tokenRefreshing ? 'refreshing' : status,
            }
          : account,
      )
      accounts.value = sortAccountsByStatusIfActive(nextAccounts)
    }

    if (current.status === status)
      return
    accountSummary.value = {
      ...accountSummary.value,
      active: Math.max(
        0,
        accountSummary.value.active
        + Number(status === 'active')
        - Number(current.status === 'active'),
      ),
      quotaExhausted: Math.max(
        0,
        accountSummary.value.quotaExhausted
        + Number(status === 'quota_exhausted')
        - Number(current.status === 'quota_exhausted'),
      ),
      attention: Math.max(
        0,
        accountSummary.value.attention
        + Number(attentionAccountStatuses.has(status))
        - Number(attentionAccountStatuses.has(current.status)),
      ),
    }
  }

  function sortAccountsByStatusIfActive(rows: AccountRow[]) {
    const sort = options.sort.value
    if (sort?.key !== 'status')
      return rows
    const direction = sort.direction === 'asc' ? 1 : -1
    return [...rows].sort((left, right) => {
      const rankDifference
        = (accountStatusSortRank[left.status] ?? 5) - (accountStatusSortRank[right.status] ?? 5)
      return rankDifference === 0
        ? left.id.localeCompare(right.id) * direction
        : rankDifference * direction
    })
  }

  async function handleToggleSchedule(account: AccountRow) {
    const nextStatus = account.status === 'disabled' ? 'active' : 'disabled'
    await updatingStatusAccounts.run(account.id, async () => {
      try {
        await updateAccount({ id: account.id, status: nextStatus })
        await loadAccounts()
        toast.success(nextStatus === 'disabled' ? '已禁用调度' : '已启用调度')
      }
      catch (error: unknown) {
        toast.error(errorMessage(error, '状态更新失败'))
      }
    })
  }

  function scheduleActionLabel(account: AccountRow) {
    return account.status === 'disabled' ? '启用调度' : '禁用调度'
  }

  function exportFileName(selectedCount: number) {
    return `cpr-accounts-selected-${selectedCount}-${dayjs().format('YYYY-MM-DD')}.json`
  }

  return {
    ...onboarding,
    showDeleteModal,
    showSingleDeleteModal,
    pendingDeleteAccount,
    refreshingAccountIds,
    refreshingQuotaAccountIds,
    updatingStatusAccountIds,
    deletingAccount,
    batchDeleting,
    exportingAccounts,
    requestDeleteAccount,
    handleDelete,
    handleBatchDelete,
    handleExportAccounts,
    handleRefresh,
    handleRefreshQuota,
    patchAccountStatus,
    handleToggleSchedule,
    scheduleActionLabel,
  }
}
