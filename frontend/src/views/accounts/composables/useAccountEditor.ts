import type { Account, AccountModelAccess, ApiKeyConfiguration } from '@/api'
import { computed, ref, shallowRef, watch } from 'vue'

import { getAccountDetail, updateAccount } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useRequestState } from '@/composables/useRequestState'
import { t } from '@/i18n'
import { notification as ZNotification } from '@/utils/notification'
import { accountModelAccessError } from '../utils/modelAccess'
import { concurrencyLimitInput, parseAccountSchedulingForm } from '../utils/schedulingForm'
import { apiKeyAccountError, emptyApiKeyAccountForm, isOpenAiApiKeyAccount, isOpenAiOAuthAccount, parseApiKeyConfiguration } from '../utils/upstreamApiKey'

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
  const apiKey = ref(emptyApiKeyAccountForm())
  const configurationRequest = useRequestState(() => ZNotification.error({
    message: t('accounts.editor.loadError'),
  }))
  const savedConfiguration = shallowRef<ApiKeyConfiguration>()
  const oauthTransport = shallowRef<ApiKeyConfiguration['transport'] | ''>('')
  const savedOAuthTransport = shallowRef<ApiKeyConfiguration['transport']>()
  const configurationReady = computed(() => !!savedConfiguration.value || !!savedOAuthTransport.value)

  async function loadConfiguration(accountId: string) {
    const requestId = configurationRequest.start()
    try {
      const detail = await getAccountDetail({ accountId }, { signal: configurationRequest.signal, silent: true })
      if (!configurationRequest.isCurrent(requestId))
        return
      if (isOpenAiOAuthAccount(detail.account)) {
        const transport = detail.credentialConfiguration?.transport
        if (transport !== 'http' && transport !== 'prefer_websocket')
          throw new Error(t('accounts.editor.oauthUnavailable'))
        oauthTransport.value = transport
        savedOAuthTransport.value = transport
        return
      }
      const configuration = parseApiKeyConfiguration(detail.credentialConfiguration)
      if (!configuration)
        throw new Error(t('accounts.editor.apiKeyUnavailable'))
      apiKey.value = {
        ...emptyApiKeyAccountForm(),
        ...configuration,
      }
      savedConfiguration.value = configuration
    }
    catch (error) {
      configurationRequest.fail(requestId, error)
    }
    finally {
      configurationRequest.finish(requestId)
    }
  }

  function open(account: Account) {
    clearCredentials()
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
    if (isOpenAiApiKeyAccount(account) || isOpenAiOAuthAccount(account))
      void loadConfiguration(account.id)
  }

  async function save() {
    const accountId = editingAccount.value?.id
    if (!accountId || saving.value)
      return
    if (new TextEncoder().encode(name.value).length > 512 || /\p{Cc}/u.test(name.value)) {
      ZNotification.warning({
        message: t('accounts.editor.nameInvalid'),
      })
      return
    }
    const isApiKey = isOpenAiApiKeyAccount(editingAccount.value)
    const isOAuth = isOpenAiOAuthAccount(editingAccount.value)
    const configuration = isApiKey && configurationReady.value
      ? parseApiKeyConfiguration(apiKey.value)
      : undefined
    if (isApiKey && !configuration)
      return
    if (isApiKey) {
      const error = apiKeyAccountError(apiKey.value, true)
      if (error) {
        ZNotification.warning({ message: error })
        return
      }
    }
    const modelError = accountModelAccessError(modelAccess.value)
    if (modelError) {
      ZNotification.warning({ message: modelError })
      return
    }
    const scheduling = parseAccountSchedulingForm(concurrencyLimit.value, weight.value)
    if (proxyMode.value === 'proxy' && !proxyId.value.trim()) {
      ZNotification.warning({
        message: t('accounts.batchEdit.proxyRequired'),
      })
      return
    }
    if (!scheduling.valid) {
      ZNotification.warning({ message: scheduling.message })
      return
    }

    await saveAction.run(async () => {
      const settings = {
        accountId,
        name: name.value.trim() === editingAccount.value?.name
          ? undefined
          : name.value.trim(),
        notes: notes.value,
        outboundProxyId: proxyMode.value === 'preserve'
          ? undefined
          : proxyMode.value === 'direct'
            ? ''
            : proxyId.value.trim(),
        enabled: schedulingEnabled.value,
        concurrencyLimit: scheduling.values.concurrencyLimit,
        weight: scheduling.values.weight,
        modelAccess: modelAccess.value,
        groupIds: [
          ...new Set(selectedGroupIds.value),
        ],
      }
      const connectionChanged = isApiKey && (
        apiKey.value.apiKey !== ''
        || apiKey.value.base_url.trim() !== savedConfiguration.value?.base_url
        || apiKey.value.transport !== savedConfiguration.value?.transport
      )
      await updateAccount({
        ...settings,
        connection: connectionChanged && configuration
          ? {
              baseUrl: configuration.base_url.trim(),
              transport: configuration.transport,
              apiKey: apiKey.value.apiKey || undefined,
            }
          : isOAuth && configurationReady.value && oauthTransport.value && oauthTransport.value !== savedOAuthTransport.value
            ? { transport: oauthTransport.value }
            : undefined,
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

  watch(showEditModal, (open) => {
    if (!open)
      configurationRequest.invalidate({ resetLoading: false })
  })

  function clearCredentials() {
    configurationRequest.invalidate()
    apiKey.value = {
      ...emptyApiKeyAccountForm(),
      transport: '',
    }
    savedConfiguration.value = undefined
    oauthTransport.value = ''
    savedOAuthTransport.value = undefined
  }

  return {
    apiKey,
    oauthTransport,
    configurationReady,
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
    clearCredentials,
  }
}
