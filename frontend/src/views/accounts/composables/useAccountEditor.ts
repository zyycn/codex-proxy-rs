import type { Ref } from 'vue'
import type { getAccounts } from '@/api'

import { computed, ref, shallowRef, watch } from 'vue'
import { revealOutboundProxy, updateAccount } from '@/api'
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
  const proxyMode = shallowRef('preserve')
  const proxyUrl = shallowRef('')
  const proxyId = shallowRef('')
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
    proxyMode.value = 'preserve'
    proxyUrl.value = ''
    proxyId.value = ''
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
    if (proxyMode.value === 'proxy' && !proxyUrl.value.trim()) {
      toast.warning('请输入代理 URL')
      return
    }
    if (proxyMode.value === 'pool' && !proxyId.value) {
      toast.warning('请选择 IP 池中的代理')
      return
    }
    if (!scheduling.valid) {
      toast.warning(scheduling.message)
      return
    }

    await saveAction.run(async () => {
      // 池内代理仅存脱敏 endpoint，提交前 reveal 完整 URL。
      const outboundProxyUrl = proxyMode.value === 'preserve'
        ? undefined
        : proxyMode.value === 'direct'
          ? ''
          : proxyMode.value === 'pool'
            ? (await revealOutboundProxy({ id: proxyId.value })).url
            : proxyUrl.value.trim()
      await updateAccount({
        accountId,
        outboundProxyUrl,
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
    proxyMode.value = 'preserve'
    proxyUrl.value = ''
    proxyId.value = ''
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
    proxyMode,
    proxyUrl,
    proxyId,
    selectedGroupIds,
    saving,
    open,
    save,
  }
}
