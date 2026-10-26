import type { ApiKey } from '@/api'
import { onScopeDispose, shallowRef } from 'vue'
import { revealApiKey } from '@/api'
import { useCopyText } from '@/composables/useCopyText'
import { useIdSet } from '@/composables/useIdSet'
import { useRequestState } from '@/composables/useRequestState'
import { t } from '@/i18n'
import { buildCodexCcSwitchImportDeeplink, resolveServiceRootUrl } from '@/utils/client'
import { notification as ZNotification } from '@/utils/notification'

// 明文读取、复制、展示与导入共用生命周期，列表数据不保存明文。
export function useApiKeyUse() {
  const copyText = useCopyText()
  const revealingKeys = useIdSet<string>()
  const revealRequest = useRequestState()
  const showUseKeyModal = shallowRef(false)
  const selectedUseKey = shallowRef<(ApiKey & { key: string }) | null>(null)
  const showCreatedKeyModal = shallowRef(false)
  const createdKey = shallowRef('')
  const createdKeyName = shallowRef('')
  const showImportKeyModal = shallowRef(false)
  const selectedImportKey = shallowRef<ApiKey | null>(null)

  function showCreatedKey(key: string, name: string) {
    createdKey.value = key
    createdKeyName.value = name
    showCreatedKeyModal.value = true
  }

  const openAiBaseUrl = `${resolveServiceRootUrl()}/v1`

  function importCreatedKey() {
    if (!createdKey.value)
      return

    window.location.href = buildCodexCcSwitchImportDeeplink({
      apiKey: createdKey.value,
      baseUrl: openAiBaseUrl,
      providerName: createdKeyName.value || 'codex-proxy-rs',
    })
  }

  function openUseKeyModal(apiKey: ApiKey) {
    return withPlaintextKey(apiKey, (key) => {
      selectedUseKey.value = { ...apiKey, key }
      showUseKeyModal.value = true
    })
  }

  function openImportKeyModal(apiKey: ApiKey) {
    revealRequest.invalidate()
    selectedImportKey.value = apiKey
    showImportKeyModal.value = true
  }

  function importSelectedKey() {
    const apiKey = selectedImportKey.value
    if (!apiKey)
      return
    return withPlaintextKey(apiKey, (key) => {
      window.location.href = buildCodexCcSwitchImportDeeplink({
        apiKey: key,
        baseUrl: openAiBaseUrl,
        providerName: apiKey.name || apiKey.prefix || 'codex-proxy-rs',
      })
    })
  }

  async function copyToClipboard(text: string) {
    await copyText(text, {
      successText: t('keys.configuration.copiedToClipboard'),
      emptyErrorText: t('common.errors.copy'),
    })
  }

  async function withPlaintextKey(apiKey: ApiKey, use: (key: string) => void | Promise<void>) {
    if (revealingKeys.has(apiKey.id))
      return
    // 明文操作共用一个当前意图，跨行点击和页面离开都使旧结果失效
    const requestId = revealRequest.start()
    try {
      const result = await revealingKeys.run(apiKey.id, () => revealApiKey({ id: apiKey.id }, { signal: revealRequest.signal }))
      if (!result || !revealRequest.isCurrent(requestId))
        return
      if (!result.plaintextKey) {
        ZNotification.error({
          message: t('keys.configuration.fullApiKeyNotAvailable'),
        })
        return
      }
      await use(result.plaintextKey)
    }
    catch (error) {
      revealRequest.fail(requestId, error)
    }
    finally {
      revealRequest.finish(requestId)
    }
  }

  function copyApiKey(apiKey: ApiKey) {
    return withPlaintextKey(apiKey, copyToClipboard)
  }

  function clearCreatedKey() {
    createdKey.value = ''
    createdKeyName.value = ''
  }

  onScopeDispose(() => {
    selectedUseKey.value = null
    selectedImportKey.value = null
    clearCreatedKey()
  })

  return {
    showUseKeyModal,
    selectedUseKey,
    showCreatedKeyModal,
    createdKey,
    showImportKeyModal,
    selectedImportKey,
    revealingKeyIds: revealingKeys.ids,
    openAiBaseUrl,
    showCreatedKey,
    clearCreatedKey,
    copyToClipboard,
    copyApiKey,
    importCreatedKey,
    openUseKeyModal,
    openImportKeyModal,
    importSelectedKey,
  }
}
