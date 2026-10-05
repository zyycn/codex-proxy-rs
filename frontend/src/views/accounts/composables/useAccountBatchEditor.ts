import type { Ref } from 'vue'
import type { Account, AccountModelAccess } from '@/api'

import { toast } from '@codex-proxy/ui'
import { computed, ref, shallowRef, watch } from 'vue'
import { batchUpdateAccounts } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { accountModelAccessError } from '../utils/modelAccess'
import { concurrencyLimitInput, parseAccountSchedulingForm } from '../utils/schedulingForm'

export function useAccountBatchEditor(options: {
  accounts: Ref<Account[]>
  selectedIds: Ref<Set<string>>
  reloadAccounts: () => Promise<unknown>
  reloadGroups: () => Promise<unknown>
}) {
  const selectedAccountsById = new Map<string, Account>()
  const showBatchEditModal = shallowRef(false)
  const editingCount = shallowRef(0)
  const schedulingEnabled = shallowRef(true)
  const concurrencyLimit = shallowRef('')
  const weight = shallowRef('1')
  const modelAccess = ref<AccountModelAccess | undefined>()
  const editedFields = ref(new Set<'enabled' | 'concurrencyLimit' | 'weight' | 'groupIds'>())
  const catalogAccountId = shallowRef<string>()
  const proxyMode = shallowRef('preserve')
  const proxyId = shallowRef('')
  const selectedGroupIds = ref<string[]>([])
  const saveAction = useAsyncAction()
  const saving = saveAction.loading
  const hasChanges = computed(() => Boolean(modelAccess.value) || editedFields.value.size > 0 || proxyMode.value !== 'preserve')

  // 未操作的字段保持每个账号原值，避免展开表单就覆盖混合设置。
  watch([schedulingEnabled, concurrencyLimit, weight, selectedGroupIds], (values, previous) => {
    if (!showBatchEditModal.value || saving.value)
      return
    const fields = ['enabled', 'concurrencyLimit', 'weight', 'groupIds'] as const
    fields.forEach((field, index) => {
      if (values[index] !== previous[index])
        editedFields.value.add(field)
    })
  }, { flush: 'sync' })

  function open() {
    const accounts = selectedAccounts()
    if (accounts.length === 0)
      return

    editingCount.value = accounts.length
    modelAccess.value = undefined
    editedFields.value.clear()
    catalogAccountId.value = accounts[0]?.id
    schedulingEnabled.value = accounts.every(account => account.enabled)
    proxyMode.value = 'preserve'
    proxyId.value = ''
    concurrencyLimit.value = sharedConcurrencyLimit(accounts)
    weight.value = sharedWeight(accounts)
    selectedGroupIds.value = sharedGroupIds(accounts)
    showBatchEditModal.value = true
  }

  async function save() {
    if (saving.value || options.selectedIds.value.size === 0)
      return
    const modelError = accountModelAccessError(modelAccess.value)
    if (modelError) {
      toast.warning(modelError)
      return
    }
    if (!hasChanges.value) {
      toast.warning('请选择需要更新的设置')
      return
    }
    const scheduling = parseAccountSchedulingForm(editedFields.value.has('concurrencyLimit') ? concurrencyLimit.value : '', editedFields.value.has('weight') ? weight.value : '1')
    if (proxyMode.value === 'proxy' && !proxyId.value.trim()) {
      toast.warning('请选择已通过测试的代理')
      return
    }
    if (!scheduling.valid) {
      toast.warning(scheduling.message)
      return
    }

    await saveAction.run(async () => {
      const accountIds = selectedAccounts().map(account => account.id)
      await batchUpdateAccounts({
        accountIds,
        modelAccess: modelAccess.value,
        outboundProxyId: proxyMode.value === 'preserve' ? undefined : proxyMode.value === 'direct' ? '' : proxyId.value.trim(),
        enabled: editedFields.value.has('enabled') ? schedulingEnabled.value : undefined,
        concurrencyLimit: editedFields.value.has('concurrencyLimit') ? scheduling.values.concurrencyLimit : undefined,
        weight: editedFields.value.has('weight') ? scheduling.values.weight : undefined,
        groupIds: editedFields.value.has('groupIds') ? [...new Set(selectedGroupIds.value)] : undefined,
      })
      showBatchEditModal.value = false
      options.selectedIds.value = new Set()
      toast.success(`已更新 ${accountIds.length} 个账号`)
      void Promise.allSettled([options.reloadAccounts(), options.reloadGroups()])
    }, { onError: () => void options.reloadAccounts() })
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

  return {
    showBatchEditModal,
    editingCount,
    schedulingEnabled,
    concurrencyLimit,
    weight,
    modelAccess,
    hasChanges,
    catalogAccountId,
    proxyMode,
    proxyId,
    selectedGroupIds,
    saving,
    open,
    save,
  }
}

function sharedConcurrencyLimit(accounts: Account[]) {
  const first = accounts[0]?.concurrencyLimit ?? null
  return accounts.every(account => account.concurrencyLimit === first)
    ? concurrencyLimitInput(first)
    : ''
}

function sharedWeight(accounts: Account[]) {
  const first = accounts[0]?.weight ?? 1
  return accounts.every(account => account.weight === first) ? String(first) : '1'
}

function sharedGroupIds(accounts: Account[]) {
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
