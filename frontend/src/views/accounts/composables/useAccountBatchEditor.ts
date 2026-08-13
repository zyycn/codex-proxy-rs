import type { Ref } from 'vue'
import type { getAccounts } from '@/api'

import { ref, shallowRef, watch } from 'vue'
import { batchUpdateAccounts } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { concurrencyLimitInput, parseAccountSchedulingForm } from '../utils/schedulingForm'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]

export function useAccountBatchEditor(options: {
  accounts: Ref<AccountRow[]>
  selectedIds: Ref<Set<string>>
  reloadAccounts: () => Promise<unknown>
  reloadGroups: () => Promise<unknown>
}) {
  const selectedAccountsById = new Map<string, AccountRow>()
  const showBatchEditModal = shallowRef(false)
  const schedulingEnabled = shallowRef(true)
  const concurrencyLimit = shallowRef('')
  const weight = shallowRef('1')
  const selectedGroupIds = ref<string[]>([])
  const saveAction = useAsyncAction()
  const saving = saveAction.loading

  function open() {
    const accounts = selectedAccounts()
    if (accounts.length === 0)
      return

    schedulingEnabled.value = accounts.every(account => account.enabled)
    concurrencyLimit.value = sharedConcurrencyLimit(accounts)
    weight.value = sharedWeight(accounts)
    selectedGroupIds.value = sharedGroupIds(accounts)
    showBatchEditModal.value = true
  }

  async function save() {
    if (saving.value || options.selectedIds.value.size === 0)
      return
    const scheduling = parseAccountSchedulingForm(concurrencyLimit.value, weight.value)
    if (!scheduling.valid) {
      toast.warning(scheduling.message)
      return
    }

    await saveAction.run(async () => {
      const accountIds = selectedAccounts().map(account => account.id)
      await batchUpdateAccounts({
        accountIds,
        enabled: schedulingEnabled.value,
        concurrencyLimit: scheduling.values.concurrencyLimit,
        weight: scheduling.values.weight,
        groupIds: [...new Set(selectedGroupIds.value)],
      })
      showBatchEditModal.value = false
      options.selectedIds.value = new Set()
      await Promise.all([options.reloadAccounts(), options.reloadGroups()])
      toast.success(`已更新 ${accountIds.length} 个账号`)
    }, { errorText: '批量更新账号失败', onError: () => void options.reloadAccounts() })
  }

  function selectedAccounts() {
    return [...options.selectedIds.value].map((accountId) => {
      const account = selectedAccountsById.get(accountId)
      if (!account)
        throw new Error(`账号 ${accountId} 的页面数据已失效，请重新选择`)
      return account
    })
  }

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

  watch([showBatchEditModal, saving], ([open, isSaving]) => {
    if (open || isSaving)
      return
    schedulingEnabled.value = true
    concurrencyLimit.value = ''
    weight.value = '1'
    selectedGroupIds.value = []
  })

  return {
    showBatchEditModal,
    schedulingEnabled,
    concurrencyLimit,
    weight,
    selectedGroupIds,
    saving,
    open,
    save,
  }
}

function sharedConcurrencyLimit(accounts: AccountRow[]) {
  const first = accounts[0]?.concurrencyLimit ?? null
  return accounts.every(account => account.concurrencyLimit === first)
    ? concurrencyLimitInput(first)
    : ''
}

function sharedWeight(accounts: AccountRow[]) {
  const first = accounts[0]?.weight ?? 1
  return accounts.every(account => account.weight === first) ? String(first) : '1'
}

function sharedGroupIds(accounts: AccountRow[]) {
  const [first, ...rest] = accounts
  if (!first)
    return []

  const shared = new Set(first.groups.map(group => group.id))
  for (const account of rest) {
    const current = new Set(account.groups.map(group => group.id))
    for (const groupId of shared) {
      if (!current.has(groupId))
        shared.delete(groupId)
    }
  }
  return [...shared]
}
