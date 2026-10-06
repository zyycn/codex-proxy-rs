import type { Account, AccountModelAccess, ApiKeyConfiguration } from '@/api'

import { toast } from '@codex-proxy/ui'
import { ref, shallowRef, watch } from 'vue'
import { getAccountDetail, updateAccount } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useRequestState } from '@/composables/useRequestState'
import { accountModelAccessError } from '../utils/modelAccess'
import { concurrencyLimitInput, parseAccountSchedulingForm } from '../utils/schedulingForm'
import { apiKeyAccountError, emptyApiKeyAccountForm, isOpenAiApiKeyAccount, isOpenAiOAuthAccount, parseApiKeyConfiguration, parseOpenAiConnectionConfiguration } from '../utils/upstreamApiKey'
import { parseWebsocketMaxAgeSeconds, websocketMaxAgeSeconds } from '../utils/websocketAge'

export function useAccountEditor(options: {
  reloadAccounts: () => Promise<unknown>
  reloadGroups: () => Promise<unknown>
}) {
  const showEditModal = shallowRef(false)
  // 保存后列表可能因筛选移除该账号，编辑窗口的退场仍需保留原账号内容。
  const editingAccount = shallowRef<Account | null>(null)
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
  const configurationRequest = useRequestState()
  const configurationLoading = configurationRequest.loading
  const configurationReady = shallowRef(false)
  const savedConfiguration = shallowRef<ApiKeyConfiguration>()
  const oauthTransport = shallowRef<ApiKeyConfiguration['transport']>('prefer_websocket')
  const oauthWebsocketMaxAgeSeconds = shallowRef('')
  const savedOAuthConfiguration = shallowRef<Omit<ApiKeyConfiguration, 'base_url'>>()

  async function loadConfiguration(accountId: string) {
    const requestId = configurationRequest.start()
    try {
      const detail = await getAccountDetail({ accountId }, { signal: configurationRequest.signal })
      if (!configurationRequest.isCurrent(requestId))
        return
      if (isOpenAiOAuthAccount(detail.account)) {
        const configuration = parseOpenAiConnectionConfiguration(detail.credentialConfiguration)
        if (!configuration)
          throw new Error('该账号没有 OAuth 上游设置')
        oauthTransport.value = configuration.transport
        oauthWebsocketMaxAgeSeconds.value = websocketMaxAgeSeconds(configuration.websocket_max_age_ms)
        savedOAuthConfiguration.value = configuration
        configurationReady.value = true
        return
      }
      const configuration = parseApiKeyConfiguration(detail.credentialConfiguration)
      if (!configuration)
        throw new Error('该账号没有 API Key 上游设置')
      apiKey.value = {
        ...emptyApiKeyAccountForm(),
        base_url: configuration.base_url,
        transport: configuration.transport,
        websocketMaxAgeSeconds: websocketMaxAgeSeconds(configuration.websocket_max_age_ms),
      }
      savedConfiguration.value = configuration
      configurationReady.value = true
    }
    catch (error) {
      configurationRequest.fail(requestId, error)
    }
    finally {
      configurationRequest.finish(requestId)
    }
  }

  function open(account: Account) {
    configurationRequest.invalidate()
    editingAccount.value = account
    notes.value = account.notes ?? ''
    proxyMode.value = 'preserve'
    proxyId.value = ''
    schedulingEnabled.value = account.enabled
    concurrencyLimit.value = concurrencyLimitInput(account.concurrencyLimit)
    weight.value = String(account.weight)
    modelAccess.value = { ...account.modelAccess, models: [...account.modelAccess.models] }
    selectedGroupIds.value = account.groups.map(group => group.id)
    apiKey.value = emptyApiKeyAccountForm()
    oauthTransport.value = 'prefer_websocket'
    oauthWebsocketMaxAgeSeconds.value = ''
    savedOAuthConfiguration.value = undefined
    savedConfiguration.value = undefined
    configurationReady.value = false
    showEditModal.value = true
    if (isOpenAiApiKeyAccount(account) || isOpenAiOAuthAccount(account))
      void loadConfiguration(account.id)
  }

  async function save() {
    const accountId = editingAccount.value?.id
    if (!accountId || saving.value)
      return
    const isApiKey = isOpenAiApiKeyAccount(editingAccount.value)
    const isOAuth = isOpenAiOAuthAccount(editingAccount.value)
    if (isApiKey && !configurationReady.value)
      return
    if (isApiKey) {
      const error = apiKeyAccountError(apiKey.value, true)
      if (error) {
        toast.warning(error)
        return
      }
    }
    const savedAge = isApiKey ? savedConfiguration.value?.websocket_max_age_ms : savedOAuthConfiguration.value?.websocket_max_age_ms
    const usesWebsocket = isApiKey ? apiKey.value.transport === 'prefer_websocket' : oauthTransport.value === 'prefer_websocket'
    const age = configurationReady.value && usesWebsocket
      ? parseWebsocketMaxAgeSeconds(isApiKey ? apiKey.value.websocketMaxAgeSeconds : oauthWebsocketMaxAgeSeconds.value)
      : { valid: true as const, value: savedAge ?? null }
    if (!age.valid) {
      toast.warning(age.message)
      return
    }
    const ageChanged = age.value !== (savedAge ?? null)
    const modelError = accountModelAccessError(modelAccess.value)
    if (modelError) {
      toast.warning(modelError)
      return
    }
    const scheduling = parseAccountSchedulingForm(concurrencyLimit.value, weight.value)
    if (proxyMode.value === 'proxy' && !proxyId.value.trim()) {
      toast.warning('请选择已通过测试的代理')
      return
    }
    if (!scheduling.valid) {
      toast.warning(scheduling.message)
      return
    }

    await saveAction.run(async () => {
      const settings = {
        accountId,
        notes: notes.value,
        outboundProxyId: proxyMode.value === 'preserve' ? undefined : proxyMode.value === 'direct' ? '' : proxyId.value.trim(),
        enabled: schedulingEnabled.value,
        concurrencyLimit: scheduling.values.concurrencyLimit,
        weight: scheduling.values.weight,
        modelAccess: modelAccess.value,
        groupIds: [...new Set(selectedGroupIds.value)],
      }
      const connectionChanged = isApiKey && (
        apiKey.value.apiKey !== ''
        || apiKey.value.base_url.trim() !== savedConfiguration.value?.base_url
        || apiKey.value.transport !== savedConfiguration.value?.transport
        || ageChanged
      )
      await updateAccount({
        ...settings,
        connection: connectionChanged
          ? {
              baseUrl: apiKey.value.base_url.trim(),
              transport: apiKey.value.transport,
              apiKey: apiKey.value.apiKey || undefined,
              websocketMaxAgeMs: ageChanged ? age.value : undefined,
            }
          : isOAuth && configurationReady.value && (oauthTransport.value !== savedOAuthConfiguration.value?.transport || ageChanged)
            ? { transport: oauthTransport.value, websocketMaxAgeMs: ageChanged ? age.value : undefined }
            : undefined,
      })
      showEditModal.value = false
      toast.success('账号已更新')
      void Promise.allSettled([options.reloadAccounts(), options.reloadGroups()])
    })
  }

  watch(showEditModal, (open) => {
    if (!open)
      configurationRequest.invalidate({ resetLoading: false })
  })

  function clearCredentials() {
    configurationRequest.invalidate()
    apiKey.value = emptyApiKeyAccountForm()
    savedConfiguration.value = undefined
    oauthWebsocketMaxAgeSeconds.value = ''
    savedOAuthConfiguration.value = undefined
  }

  return {
    apiKey,
    oauthTransport,
    oauthWebsocketMaxAgeSeconds,
    configurationLoading,
    configurationReady,
    showEditModal,
    editingAccount,
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
