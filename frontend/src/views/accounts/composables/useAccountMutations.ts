import type { Ref } from 'vue'
import type { getAccounts } from '@/api'
import type { BaseTableSort } from '@/components/base/BaseTable/columns'

import dayjs from 'dayjs'
import { ref, watch } from 'vue'
import {
  deleteAccounts,
  disableAccount,
  enableAccount,
  exportAccounts,
  getAccountQuota,
  refreshAccount,
  refreshAccountQuota,
} from '@/api'
import { ApiError } from '@/api/request'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useDownload } from '@/composables/useDownload'
import { useIdSet } from '@/composables/useIdSet'
import { errorMessage, withMinimumDuration } from '@/utils/async'

import { useAccountOnboarding } from './useAccountOnboarding'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]

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
  accountSummary: Ref<Awaited<ReturnType<typeof getAccounts>>['summary']>
  statusQuery: Ref<string>
  sort: Ref<BaseTableSort | undefined>
  selectedIds: Ref<Set<string>>
  totalAccounts: Ref<number>
  configRevision: Ref<number>
  reload: () => Promise<unknown>
}) {
  const loadAccounts = options.reload
  const { downloadJson } = useDownload()
  const onboarding = useAccountOnboarding({
    reload: loadAccounts,
    configRevision: options.configRevision,
  })
  const selectedAccountsById = new Map<string, AccountRow>()
  const showDeleteModal = ref(false)
  const showSingleDeleteModal = ref(false)
  const pendingDeleteAccount = ref<AccountRow | null>(null)
  const updatingStatusAccounts = useIdSet<string>()
  const refreshingAccounts = useIdSet<string>()
  const refreshingQuotaAccounts = useIdSet<string>()
  const deletingAccountAction = useAsyncAction()
  const batchDeletingAction = useAsyncAction()
  const exportingAccountsAction = useAsyncAction()
  const updatingStatusAccountIds = updatingStatusAccounts.ids
  const refreshingAccountIds = refreshingAccounts.ids
  const refreshingQuotaAccountIds = refreshingQuotaAccounts.ids
  const deletingAccount = deletingAccountAction.loading
  const batchDeleting = batchDeletingAction.loading
  const exportingAccounts = exportingAccountsAction.loading

  watch(
    [options.accounts, options.selectedIds],
    ([accounts, selectedIds]) => {
      for (const account of accounts) {
        if (selectedIds.has(account.id))
          selectedAccountsById.set(account.id, account)
      }
      for (const accountId of selectedAccountsById.keys()) {
        if (!selectedIds.has(accountId))
          selectedAccountsById.delete(accountId)
      }
    },
    { immediate: true, flush: 'sync' },
  )

  function requestDeleteAccount(account: AccountRow) {
    pendingDeleteAccount.value = account
    showSingleDeleteModal.value = true
  }

  async function handleDelete() {
    const account = pendingDeleteAccount.value
    if (deletingAccount.value || !account)
      return

    await deletingAccountAction.run(
      async () => {
        await deleteAccountBatch([account])
        const remaining = new Set(options.selectedIds.value)
        remaining.delete(account.id)
        options.selectedIds.value = remaining
        showSingleDeleteModal.value = false
        pendingDeleteAccount.value = null
        await loadAccounts()
        toast.success('账号已删除')
      },
      { errorText: '删除失败' },
    )
  }

  async function handleBatchDelete() {
    if (batchDeleting.value || options.selectedIds.value.size === 0)
      return

    let deletedCount = 0
    await batchDeletingAction.run(
      async () => {
        const selected = accountsById([...options.selectedIds.value])
        for (const accounts of accountDeletionGroups(selected)) {
          await deleteAccountBatch(accounts)
          deletedCount += accounts.length
          const deletedIds = new Set(accounts.map(account => account.id))
          const remaining = new Set(options.selectedIds.value)
          for (const accountId of deletedIds)
            remaining.delete(accountId)
          options.selectedIds.value = remaining
        }
        showDeleteModal.value = false
        await loadAccounts()
        toast.success(`已删除 ${deletedCount} 个账号`)
      },
      {
        errorText: false,
        onError: (error) => {
          void loadAccounts().catch(() => undefined)
          toast.error(
            deletedCount > 0
              ? `已删除 ${deletedCount} 个账号，其余未删除：${errorMessage(error, '操作失败')}`
              : errorMessage(error, '批量删除失败'),
          )
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
          accountIds: selected.join(','),
          confirm: 'export_sensitive_accounts',
        })
        const fileName = `cpr-accounts-selected-${selected.length}-${dayjs().format('YYYY-MM-DD')}.json`
        await downloadJson(payload, fileName)
        toast.success(`已导出 ${selected.length} 个账号`)
      },
      { errorText: '导出失败' },
    )
  }

  async function handleRefresh(accountId: string) {
    await refreshingAccounts.run(accountId, async () => {
      try {
        const result = await withMinimumDuration(() =>
          refreshAccount({
            accountId,
            expectedConfigRevision: options.configRevision.value,
          }),
        )
        onboarding.commitConfigRevision(result.configRevision)
        await loadAccounts()
        if (result.result === 'skipped') {
          toast.warning(result.error || 'Token 正在刷新中')
          return
        }
        if (result.result === 'failed') {
          toast.error(result.error || '刷新失败')
          return
        }
        toast.success('Token 已刷新')
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
          await refreshAccountQuota({ accountId })
          const result = await getAccountQuota({ accountId })
          options.accounts.value = options.accounts.value.map(account =>
            account.id === accountId ? { ...account, ...result.account } : account,
          )
        })
        toast.success('额度已刷新')
      }
      catch (error: unknown) {
        toast.error(errorMessage(error, '额度刷新失败'))
      }
    })
  }

  function patchAccountStatus(accountId: string, status: string) {
    const current = options.accounts.value.find(account => account.id === accountId)
    if (!current)
      return

    if (options.statusQuery.value && options.statusQuery.value !== status) {
      options.accounts.value = options.accounts.value.filter(account => account.id !== accountId)
      options.totalAccounts.value = Math.max(0, options.totalAccounts.value - 1)
      options.selectedIds.value = new Set(
        [...options.selectedIds.value].filter(id => id !== accountId),
      )
    }
    else {
      const rows = options.accounts.value.map(account =>
        account.id === accountId
          ? {
              ...account,
              status,
              displayStatus: account.tokenRefreshing ? 'refreshing' : status,
            }
          : account,
      )
      options.accounts.value = sortAccountsByStatus(rows)
    }

    if (current.status === status)
      return
    options.accountSummary.value = {
      ...options.accountSummary.value,
      active: Math.max(
        0,
        options.accountSummary.value.active
        + Number(status === 'active')
        - Number(current.status === 'active'),
      ),
      quotaExhausted: Math.max(
        0,
        options.accountSummary.value.quotaExhausted
        + Number(status === 'quota_exhausted')
        - Number(current.status === 'quota_exhausted'),
      ),
      attention: Math.max(
        0,
        options.accountSummary.value.attention
        + Number(attentionAccountStatuses.has(status))
        - Number(attentionAccountStatuses.has(current.status)),
      ),
    }
  }

  function sortAccountsByStatus(rows: AccountRow[]) {
    if (options.sort.value?.key !== 'status')
      return rows
    const direction = options.sort.value.direction === 'asc' ? 1 : -1
    return [...rows].sort((left, right) => {
      const rankDifference
        = (accountStatusSortRank[left.status] ?? 5) - (accountStatusSortRank[right.status] ?? 5)
      return rankDifference === 0
        ? left.id.localeCompare(right.id) * direction
        : rankDifference * direction
    })
  }

  async function handleToggleSchedule(account: AccountRow) {
    const action = account.enabled ? 'disable' : 'enable'
    await updatingStatusAccounts.run(account.id, async () => {
      try {
        await mutateCredential(account, action)
        await loadAccounts()
        toast.success(action === 'disable' ? '已禁用调度' : '已启用调度')
      }
      catch (error: unknown) {
        toast.error(errorMessage(error, '状态更新失败'))
      }
    })
  }

  function scheduleActionLabel(account: AccountRow) {
    return account.enabled ? '禁用调度' : '启用调度'
  }

  function accountsById(ids: string[]) {
    const accounts = []
    for (const id of ids) {
      const account = selectedAccountsById.get(id)
      if (!account)
        throw new Error(`账号 ${id} 的页面数据已失效，请重新选择`)
      accounts.push(account)
    }
    return accounts
  }

  async function mutateCredential(account: AccountRow, action: 'enable' | 'disable') {
    const payload = {
      provider: account.provider,
      accountId: account.id,
      expectedConfigRevision: await onboarding.requireConfigRevision(),
    }
    try {
      const result = action === 'enable'
        ? await enableAccount(payload)
        : await disableAccount(payload)
      if (!result)
        throw new Error(`不支持的 Provider：${account.provider}`)
      onboarding.commitConfigRevision(result.configRevision)
    }
    catch (error) {
      if (error instanceof ApiError && error.status === 409) {
        await loadAccounts()
      }
      throw error
    }
  }

  async function deleteAccountBatch(accounts: AccountRow[]) {
    const account = accounts[0]
    if (!account)
      return
    const payload = {
      provider: account.provider,
      accountIds: accounts.map(account => account.id),
      expectedConfigRevision: await onboarding.requireConfigRevision(),
    }
    try {
      const result = await deleteAccounts(payload)
      if (!result)
        throw new Error(`不支持的 Provider：${account.provider}`)
      onboarding.commitConfigRevision(result.configRevision)
    }
    catch (error) {
      if (error instanceof ApiError && error.status === 409) {
        await loadAccounts()
      }
      throw error
    }
  }

  function accountDeletionGroups(accounts: AccountRow[]) {
    const groups = new Map<string, AccountRow[]>()
    for (const account of accounts) {
      const key = account.provider
      const group = groups.get(key)
      if (group)
        group.push(account)
      else
        groups.set(key, [account])
    }
    return groups.values()
  }

  return {
    ...onboarding,
    showDeleteModal,
    showSingleDeleteModal,
    pendingDeleteAccount,
    updatingStatusAccountIds,
    refreshingAccountIds,
    refreshingQuotaAccountIds,
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
