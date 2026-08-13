import type { Ref } from 'vue'
import type { getAccounts } from '@/api'

import { computed, ref, shallowRef, watch } from 'vue'
import { updateAccount } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { concurrencyLimitInput, parseAccountSchedulingForm } from '../utils/schedulingForm'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]

export function useAccountEditor(options: {
  accounts: Ref<AccountRow[]>
  reloadAccounts: () => Promise<unknown>
  reloadGroups: () => Promise<unknown>
}) {
  const showEditModal = shallowRef(false)
  const editingAccountId = shallowRef<string | null>(null)
  const schedulingEnabled = shallowRef(true)
  const concurrencyLimit = shallowRef('')
  const weight = shallowRef('1')
  const selectedGroupIds = ref<string[]>([])
  const saveAction = useAsyncAction()
  const saving = saveAction.loading
  const editingAccount = computed(() => {
    const accountId = editingAccountId.value
    return accountId
      ? options.accounts.value.find(account => account.id === accountId) ?? null
      : null
  })

  function open(account: AccountRow) {
    editingAccountId.value = account.id
    schedulingEnabled.value = account.enabled
    concurrencyLimit.value = concurrencyLimitInput(account.concurrencyLimit)
    weight.value = String(account.weight)
    selectedGroupIds.value = account.groups.map(group => group.id)
    showEditModal.value = true
  }

  async function save() {
    const accountId = editingAccountId.value
    if (!accountId || saving.value)
      return
    const scheduling = parseAccountSchedulingForm(concurrencyLimit.value, weight.value)
    if (!scheduling.valid) {
      toast.warning(scheduling.message)
      return
    }

    await saveAction.run(async () => {
      await updateAccount({
        accountId,
        enabled: schedulingEnabled.value,
        concurrencyLimit: scheduling.values.concurrencyLimit,
        weight: scheduling.values.weight,
        groupIds: [...new Set(selectedGroupIds.value)],
      })
      showEditModal.value = false
      await Promise.all([options.reloadAccounts(), options.reloadGroups()])
      toast.success('账号已更新')
    }, { errorText: '账号更新失败' })
  }

  watch([showEditModal, saving], ([open, isSaving]) => {
    if (open || isSaving)
      return
    editingAccountId.value = null
    schedulingEnabled.value = true
    concurrencyLimit.value = ''
    weight.value = '1'
    selectedGroupIds.value = []
  })

  return {
    showEditModal,
    editingAccount,
    schedulingEnabled,
    concurrencyLimit,
    weight,
    selectedGroupIds,
    saving,
    open,
    save,
  }
}
