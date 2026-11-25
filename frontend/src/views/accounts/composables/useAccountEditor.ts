import type { Account, AccountConnectionUpdate, AccountModelAccess } from '@/api'
import { ref, shallowRef } from 'vue'

import { updateAccount } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { t } from '@/i18n'
import { notification as ZNotification } from '@/plugins/notification'
import { accountNameUpdate } from '../utils/accountName'
import { accountSchedulingValues, concurrencyLimitInput } from '../utils/schedulingForm'

export function useAccountEditor(options: {
  reloadAccounts: () => Promise<unknown>
  reloadGroups: () => Promise<unknown>
}) {
  const showEditModal = shallowRef(false)
  // 保存后列表可能因筛选移除该账号，编辑窗口的退场仍需保留原账号内容。
  const editingAccount = shallowRef<Account | null>(null)
  const name = shallowRef('')
  const notes = shallowRef('')
  const schedulingEnabled = shallowRef(true)
  const concurrencyLimit = shallowRef('')
  const weight = shallowRef('1')
  const modelAccess = ref<AccountModelAccess | undefined>()
  const proxyMode = shallowRef('preserve')
  const proxyId = shallowRef('')
  const selectedGroupIds = ref<string[]>([])
  const saveAction = useAsyncAction()
  const saving = saveAction.loading
  function open(account: Account) {
    editingAccount.value = account
    name.value = account.name
    notes.value = account.notes ?? ''
    proxyMode.value = 'preserve'
    proxyId.value = ''
    schedulingEnabled.value = account.enabled
    concurrencyLimit.value = concurrencyLimitInput(account.concurrencyLimit)
    weight.value = String(account.weight)
    modelAccess.value = {
      ...account.modelAccess,
      models: [...account.modelAccess.models],
    }
    selectedGroupIds.value = account.groups.map(group => group.id)
    showEditModal.value = true
  }

  async function save(connection?: AccountConnectionUpdate) {
    const account = editingAccount.value
    if (!account || saving.value)
      return
    const scheduling = accountSchedulingValues(concurrencyLimit.value, weight.value)

    await saveAction.run(async () => {
      await updateAccount({
        connection,
        accountId: account.id,
        name: accountNameUpdate(name.value, account.name),
        notes: notes.value,
        outboundProxyId: proxyMode.value === 'preserve'
          ? undefined
          : proxyMode.value === 'deployment'
            ? ''
            : proxyId.value.trim(),
        enabled: schedulingEnabled.value,
        concurrencyLimit: scheduling.concurrencyLimit,
        weight: scheduling.weight,
        modelAccess: modelAccess.value,
        groupIds: [
          ...new Set(selectedGroupIds.value),
        ],
      })
      showEditModal.value = false
      ZNotification.success({
        message: t('accounts.editor.saved'),
      })
      void Promise.allSettled([
        options.reloadAccounts(),
        options.reloadGroups(),
      ])
    })
  }

  return {
    showEditModal,
    editingAccount,
    name,
    notes,
    schedulingEnabled,
    concurrencyLimit,
    weight,
    modelAccess,
    proxyMode,
    proxyId,
    selectedGroupIds,
    saving,
    open,
    save,
  }
}
