import type { ApiKeyConfiguration } from './connection'
import type { Account, AccountConnectionUpdate } from '@/api'
import { computed, ref, shallowRef, watch } from 'vue'
import { getAccountDetail } from '@/api'
import { useRequestState } from '@/composables/useRequestState'
import { t } from '@/i18n'
import { notification as ZNotification } from '@/plugins/notification'
import { errorMessage } from '@/utils/operation'
import { connectionKind, emptyApiKeyAccountForm, parseApiKeyConfiguration } from './connection'

export function useConnectionEditor(account: () => Account | null, open: () => boolean) {
  const apiKey = ref(emptyApiKeyAccountForm())
  const configurationRequest = useRequestState(cause => ZNotification.error({
    message: errorMessage(cause, t('accounts.editor.loadError')),
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
      if (connectionKind(detail.account) === 'oauth') {
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

  const kind = computed(() => connectionKind(account()))
  const canSave = computed(() => kind.value !== 'api_key' || (configurationReady.value && !!parseApiKeyConfiguration(apiKey.value)))
  const change = computed<AccountConnectionUpdate | undefined>(() => {
    if (kind.value === 'oauth' && savedOAuthTransport.value && oauthTransport.value && oauthTransport.value !== savedOAuthTransport.value)
      return { transport: oauthTransport.value }

    const saved = savedConfiguration.value
    if (kind.value !== 'api_key' || !saved)
      return undefined
    const form = apiKey.value
    if (!form.apiKey && form.base_url.trim() === saved.base_url && form.protocol === saved.protocol && form.transport === saved.transport)
      return undefined
    const configuration = parseApiKeyConfiguration(form)
    if (configuration) {
      return {
        baseUrl: configuration.base_url.trim(),
        protocol: configuration.protocol,
        transport: configuration.transport,
        apiKey: form.apiKey || undefined,
      }
    }
  })

  watch([open, account], ([visible, selected]) => {
    configurationRequest.invalidate()
    apiKey.value = {
      ...emptyApiKeyAccountForm(),
      transport: '',
    }
    savedConfiguration.value = undefined
    oauthTransport.value = ''
    savedOAuthTransport.value = undefined
    if (visible && selected && kind.value)
      void loadConfiguration(selected.id)
  }, { immediate: true })

  return {
    apiKey,
    oauthTransport,
    kind,
    configurationReady,
    canSave,
    change,
  }
}
