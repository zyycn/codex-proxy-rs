import type { ApiKey } from '@/api'
import { toast } from '@codex-proxy/ui'
import { shallowRef } from 'vue'
import { revealApiKey } from '@/api'
import { useCopyText } from '@/composables/useCopyText'
import { useIdSet } from '@/composables/useIdSet'
import { buildCodexCcSwitchImportDeeplink, resolveServiceRootUrl } from '@/utils/client'

// 明文读取、复制、展示与导入共用生命周期，列表数据不保存明文。
export function useApiKeyUse() {
  const copyText = useCopyText()
  const revealingKeys = useIdSet<string>()
  const showUseKeyModal = shallowRef(false)
  const selectedUseKey = shallowRef<(ApiKey & { key: string }) | null>(null)
  const showCreatedKeyModal = shallowRef(false)
  const createdKey = shallowRef('')
  const createdKeyName = shallowRef('')

  function showCreatedKey(key: string, name: string) {
    createdKey.value = key
    createdKeyName.value = name
    showCreatedKeyModal.value = true
  }

  const openAiBaseUrl = `${resolveServiceRootUrl()}/v1`

  function importCreatedKeyToCcs() {
    if (!createdKey.value)
      return

    window.location.href = buildCodexCcSwitchImportDeeplink({
      apiKey: createdKey.value,
      baseUrl: openAiBaseUrl,
      providerName: createdKeyName.value || 'codex-proxy-rs',
    })
  }

  async function openUseKeyModal(apiKey: ApiKey) {
    const key = await revealPlaintextKey(apiKey)
    if (!key)
      return
    selectedUseKey.value = { ...apiKey, key }
    showUseKeyModal.value = true
  }

  async function importToCcs(apiKey: ApiKey) {
    const key = await revealPlaintextKey(apiKey)
    if (!key)
      return
    window.location.href = buildCodexCcSwitchImportDeeplink({
      apiKey: key,
      baseUrl: openAiBaseUrl,
      providerName: apiKey.name || apiKey.prefix || 'codex-proxy-rs',
    })
  }

  async function copyToClipboard(text: string) {
    await copyText(text, { successText: '已复制到剪贴板', emptyErrorText: '复制失败' })
  }

  async function revealPlaintextKey(apiKey: ApiKey) {
    try {
      const result = await revealingKeys.run(apiKey.id, () => revealApiKey({ id: apiKey.id }))
      if (!result)
        return undefined
      if (!result.plaintextKey) {
        toast.error('完整 API Key 不可用')
        return undefined
      }
      return result.plaintextKey
    }
    catch {
      return undefined
    }
  }

  async function copyApiKey(apiKey: ApiKey) {
    const key = await revealPlaintextKey(apiKey)
    if (key)
      await copyToClipboard(key)
  }

  function clearCreatedKey() {
    createdKey.value = ''
    createdKeyName.value = ''
  }

  return {
    showUseKeyModal,
    selectedUseKey,
    showCreatedKeyModal,
    createdKey,
    revealingKeyIds: revealingKeys.ids,
    openAiBaseUrl,
    showCreatedKey,
    clearCreatedKey,
    copyToClipboard,
    copyApiKey,
    importCreatedKeyToCcs,
    openUseKeyModal,
    importToCcs,
  }
}
