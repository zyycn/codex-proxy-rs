import type { Ref } from 'vue'
import type { Account, AccountImportTask } from '@/api'
import type { RequestOptions } from '@/api/request'
import { toast } from '@codex-proxy/ui'
import { computed, ref, shallowReactive, watch } from 'vue'
import {
  batchUpdateAccounts,
  deleteAccounts,
  exportAccounts,
  getAccountModelCatalog,
  recoverAccount,
  refreshAccount,
  refreshAccountQuota,
} from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useDownload } from '@/composables/useDownload'
import { useIdSet } from '@/composables/useIdSet'
import { errorMessage, withMinimumDuration } from '@/utils/operation'
import { isSupportedProvider } from '@/utils/providers'

import { useAccountOnboarding } from './useAccountOnboarding'

export function useAccountMutations(options: {
  accounts: Ref<Account[]>
  selectedIds: Ref<Set<string>>
  onImportTaskCreated: (task: AccountImportTask) => void
  reload: () => Promise<unknown>
  replaceAccount: (account: Account) => Promise<boolean>
}) {
  const loadAccounts = options.reload
  const { downloadJson } = useDownload()
  const onboarding = useAccountOnboarding({
    reload: loadAccounts,
    onImportTaskCreated: options.onImportTaskCreated,
  })
  const selectedAccountsById = shallowReactive(new Map<string, Account>())
  const showDeleteModal = ref(false)
  const showSingleDeleteModal = ref(false)
  const pendingDeleteAccount = ref<Account | null>(null)
  const deleteCount = ref(0)
  const recoveringAccounts = useIdSet<string>()
  const refreshingAccounts = useIdSet<string>()
  const refreshingQuotaAccounts = useIdSet<string>()
  const downloadingCatalogAccounts = useIdSet<string>()
  const togglingSchedulingAccounts = useIdSet<string>()
  const deletingAccountAction = useAsyncAction()
  const batchDeletingAction = useAsyncAction()
  const exportingAccountsAction = useAsyncAction()
  const recoveringAccountIds = recoveringAccounts.ids
  const refreshingAccountIds = refreshingAccounts.ids
  const refreshingQuotaAccountIds = refreshingQuotaAccounts.ids
  const downloadingCatalogAccountIds = downloadingCatalogAccounts.ids
  const togglingSchedulingAccountIds = togglingSchedulingAccounts.ids
  const deletingAccount = deletingAccountAction.loading
  const batchDeleting = batchDeletingAction.loading
  const exportingAccounts = exportingAccountsAction.loading
  watch([showDeleteModal, batchDeleting], ([open, busy]) => {
    if (open && !busy)
      deleteCount.value = options.selectedIds.value.size
  })
  const exportDisabledReason = computed(() => {
    if (options.selectedIds.value.size === 0)
      return ''
    for (const id of options.selectedIds.value) {
      const account = selectedAccountsById.get(id)
      if (!account)
        return '所选账号数据已失效，请重新选择'
      if (!isSupportedProvider(account.provider))
        return '所选账号包含不支持导出的平台'
    }
    return ''
  })

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

  function requestDeleteAccount(account: Account) {
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
        await loadAccounts()
        toast.success('账号已删除')
      },
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
          await deleteAccountBatch(accounts, { silent: true })
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
    if (exportDisabledReason.value) {
      toast.warning(exportDisabledReason.value)
      return
    }

    await exportingAccountsAction.run(
      async () => {
        const payload = await exportAccounts({
          accountIds: selected.join(','),
          confirm: 'export_sensitive_accounts',
        })
        const fileName = payload.fileName
        await downloadJson(payload, fileName)
        toast.success(`已导出 ${selected.length} 个账号`)
      },
      { errorText: '导出失败' },
    )
  }

  async function handleDownloadModelCatalog(account: Account) {
    await downloadingCatalogAccounts.run(account.id, async () => {
      try {
        const result = await getAccountModelCatalog({ accountId: account.id })
        const plan = account.planTypeDisplay.trim().replace(/[^\p{L}\p{N}_-]/gu, '_') || 'unknown-plan'
        const name = account.name.trim().replace(/[^\p{L}\p{N}_-]/gu, '_') || 'account'
        await downloadJson(result.catalog, `cpr-model-catalog-${plan}-${name}.json`)
        toast.success(`已下载模型目录，共 ${result.modelCount} 个模型`)
      }
      catch {}
    })
  }

  async function handleRefresh(accountId: string) {
    await refreshingAccounts.run(accountId, async () => {
      try {
        const result = await withMinimumDuration(() =>
          refreshAccount({
            accountId,
          }),
        )
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
      catch {}
    })
  }

  async function handleToggleScheduling(account: Account) {
    await togglingSchedulingAccounts.run(account.id, async () => {
      try {
        await batchUpdateAccounts({ accountIds: [account.id], enabled: !account.enabled })
        await loadAccounts()
        toast.success(account.enabled ? '调度已停用' : '调度已启用')
      }
      catch {}
    })
  }

  async function handleRefreshQuota(accountId: string) {
    await refreshingQuotaAccounts.run(accountId, async () => {
      try {
        const result = await withMinimumDuration(() => refreshAccountQuota({ accountId }))
        const remainsVisible = await options.replaceAccount(result.account)
        if (!remainsVisible) {
          const selectedIds = new Set(options.selectedIds.value)
          selectedIds.delete(accountId)
          options.selectedIds.value = selectedIds
        }
        toast.success('额度已刷新')
      }
      catch {}
    })
  }

  async function handleQuotaReset(accountId: string) {
    if (!options.accounts.value.find(account => account.id === accountId)?.capabilities.quotaRefresh)
      return
    try {
      const result = await refreshAccountQuota({ accountId }, { silent: true })
      await options.replaceAccount(result.account)
    }
    catch (error: unknown) {
      toast.warning(
        `额度已重置，但最新额度加载失败：${errorMessage(error, '请手动刷新额度')}`,
        { duration: 5000 },
      )
    }
  }

  async function handleRecover(accountId: string) {
    await recoveringAccounts.run(accountId, async () => {
      try {
        const result = await withMinimumDuration(() => recoverAccount({ accountId }))
        const remainsVisible = await options.replaceAccount(result.account)
        if (!remainsVisible) {
          const selectedIds = new Set(options.selectedIds.value)
          selectedIds.delete(accountId)
          options.selectedIds.value = selectedIds
        }
        toast.success('账号状态已恢复')
      }
      catch {}
    })
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

  async function deleteAccountBatch(accounts: Account[], options?: RequestOptions) {
    const account = accounts[0]
    if (!account)
      return
    const payload = {
      provider: account.provider,
      accountIds: accounts.map(account => account.id),
    }
    const result = await deleteAccounts(payload, options)
    if (!result)
      throw new Error(`不支持的 Provider：${account.provider}`)
  }

  function accountDeletionGroups(accounts: Account[]) {
    const groups = new Map<string, Account[]>()
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
    deleteCount,
    recoveringAccountIds,
    refreshingAccountIds,
    refreshingQuotaAccountIds,
    downloadingCatalogAccountIds,
    togglingSchedulingAccountIds,
    deletingAccount,
    batchDeleting,
    exportingAccounts,
    exportDisabledReason,
    requestDeleteAccount,
    handleDelete,
    handleBatchDelete,
    handleExportAccounts,
    handleDownloadModelCatalog,
    handleRecover,
    handleRefresh,
    handleRefreshQuota,
    handleQuotaReset,
    handleToggleScheduling,
  }
}
