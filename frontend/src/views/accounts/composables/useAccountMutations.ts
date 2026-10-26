import type { Ref } from 'vue'
import type { Account, AccountImportTask } from '@/api'
import type { RequestOptions } from '@/api/request'
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
import { t } from '@/i18n'
import { notification as ZNotification } from '@/utils/notification'
import { errorMessage, withMinimumDuration } from '@/utils/operation'
import { isSupportedProvider } from '@/utils/providers'

import { useAccountOnboarding } from './useAccountOnboarding'

export function useAccountMutations(options: {
  accounts: Ref<Account[]>
  selectedIds: Ref<Set<string>>
  onImportTaskCreated: (task: AccountImportTask, reveal?: boolean) => void
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
        return t('accounts.actions.staleSelection')
      if (!isSupportedProvider(account.provider))
        return t('accounts.actions.exportUnsupported')
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
        ZNotification.success({
          message: t('accounts.actions.deleted'),
        })
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
        ZNotification.success({
          message: t('accounts.actions.deletedAccountDeletedAccounts', { count: deletedCount }),
        })
      },
      {
        errorText: false,
        onError: (error) => {
          void loadAccounts().catch(() => undefined)
          ZNotification.error({
            message: deletedCount > 0
              ? t('accounts.actions.partialDeletion', {
                  count: deletedCount,
                  value: errorMessage(error, t('common.errors.operation')),
                })
              : errorMessage(error, t('accounts.actions.batchDeletionFailed')),
          })
        },
      },
    )
  }

  async function handleExportAccounts() {
    if (exportingAccounts.value)
      return
    const selected = [...options.selectedIds.value]
    if (selected.length === 0) {
      ZNotification.warning({
        message: t('accounts.actions.exportSelectionRequired'),
      })
      return
    }
    if (exportDisabledReason.value) {
      ZNotification.warning({ message: exportDisabledReason.value })
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
        ZNotification.success({
          message: t('accounts.actions.exported', { count: selected.length }),
        })
      },
      {
        errorText: t('accounts.actions.exportFailed'),
      },
    )
  }

  async function handleDownloadModelCatalog(account: Account) {
    await downloadingCatalogAccounts.run(account.id, async () => {
      try {
        const result = await getAccountModelCatalog({ accountId: account.id })
        const plan = account.planTypeDisplay.trim().replace(/[^\p{L}\p{N}_-]/gu, '_') || 'unknown-plan'
        const name = account.name.trim().replace(/[^\p{L}\p{N}_-]/gu, '_') || 'account'
        await downloadJson(result.catalog, `cpr-model-catalog-${plan}-${name}.json`)
        ZNotification.success({
          message: t('accounts.actions.modelCatalogDownloadedModels', { count: result.modelCount }),
        })
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
          ZNotification.warning({ message: result.error || t('accounts.actions.tokenIsBeingRefreshed') })
          return
        }
        if (result.result === 'failed') {
          ZNotification.error({ message: result.error || t('accounts.actions.refreshFailed') })
          return
        }
        ZNotification.success({
          message: t('accounts.actions.tokenRefreshed'),
        })
      }
      catch {}
    })
  }

  async function handleToggleScheduling(account: Account) {
    await togglingSchedulingAccounts.run(account.id, async () => {
      try {
        await batchUpdateAccounts({
          accountIds: [account.id],
          enabled: !account.enabled,
        })
        await loadAccounts()
        ZNotification.success({
          message: account.enabled
            ? t('accounts.actions.schedulingIsDisabled')
            : t('accounts.actions.schedulingIsEnabled'),
        })
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
        ZNotification.success({
          message: t('accounts.actions.quotaRefreshed'),
        })
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
      ZNotification.warning({
        message: t('accounts.actions.resetRefreshFailed', {
          value: errorMessage(error, t('accounts.actions.refreshQuotaManually')),
        }),
        ...({ duration: 5000 }),
      })
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
        ZNotification.success({
          message: t('accounts.actions.statusRestored'),
        })
      }
      catch {}
    })
  }

  function accountsById(ids: string[]) {
    const accounts = []
    for (const id of ids) {
      const account = selectedAccountsById.get(id)
      if (!account)
        throw new Error(t('accounts.actions.staleAccount', { id }))
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
      throw new Error(t('accounts.actions.unsupportedProvider', { provider: account.provider }))
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
