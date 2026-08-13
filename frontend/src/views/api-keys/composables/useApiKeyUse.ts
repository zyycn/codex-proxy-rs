import type { Ref, ShallowRef } from 'vue'
import type { getApiKeys } from '@/api'
import { computed, shallowRef, watch } from 'vue'

import { API_BASE_URL } from '@/api/constants'
import { toast } from '@/components/base/BaseToast'
import { errorMessage } from '@/utils/async'
import { buildCodexCcSwitchImportDeeplink } from '../utils/ccswitchImport'

// “使用密钥”弹窗展示明文时，在列表行上补挂 reveal 得到的完整 key。
type ApiKeyRow = Awaited<ReturnType<typeof getApiKeys>>['items'][number] & { key?: string }

// 密钥使用与 CCSwitch 导入编排：服务根地址推导、deeplink 跳转、
// “使用密钥”弹窗的明文补全与打开。
export function useApiKeyUse(options: {
  createdKey: Readonly<Ref<string>>
  createdKeyName: Readonly<Ref<string>>
  revealPlaintextKey: (apiKey: ApiKeyRow) => Promise<string>
}) {
  const showUseKeyModal = shallowRef(false)
  const selectedUseKey: ShallowRef<ApiKeyRow | null> = shallowRef(null)

  const serviceRootUrl = computed(() => resolveServiceRootUrl())
  const openAiBaseUrl = computed(() => `${serviceRootUrl.value}/v1`)

  function resolveServiceRootUrl() {
    const normalizedApiBase = API_BASE_URL.trim().replace(/\/+$/, '')

    if (/^https?:\/\//i.test(normalizedApiBase)) {
      return normalizedApiBase
    }

    if (typeof window === 'undefined') {
      return normalizedApiBase
    }

    const origin = window.location.origin.replace(/\/+$/, '')
    if (!normalizedApiBase) {
      return origin
    }

    return `${origin}${normalizedApiBase.startsWith('/') ? normalizedApiBase : `/${normalizedApiBase}`}`
  }

  function importCreatedKeyToCcs() {
    if (!options.createdKey.value)
      return

    window.location.href = buildCodexCcSwitchImportDeeplink({
      apiKey: options.createdKey.value,
      baseUrl: openAiBaseUrl.value,
      providerName: options.createdKeyName.value || 'codex-proxy-rs',
    })
  }

  async function openUseKeyModal(apiKey: ApiKeyRow) {
    try {
      const key = await options.revealPlaintextKey(apiKey)
      selectedUseKey.value = { ...apiKey, key }
      showUseKeyModal.value = true
    }
    catch (error: unknown) {
      toast.error(errorMessage(error, '读取完整密钥失败'))
    }
  }

  async function importToCcs(apiKey: ApiKeyRow) {
    try {
      const key = await options.revealPlaintextKey(apiKey)
      window.location.href = buildCodexCcSwitchImportDeeplink({
        apiKey: key,
        baseUrl: openAiBaseUrl.value,
        providerName: apiKey.name || apiKey.prefix || 'codex-proxy-rs',
      })
    }
    catch (error: unknown) {
      toast.error(errorMessage(error, '读取完整密钥失败'))
    }
  }

  watch(showUseKeyModal, (open) => {
    if (!open)
      selectedUseKey.value = null
  })

  return {
    showUseKeyModal,
    selectedUseKey,
    serviceRootUrl,
    openAiBaseUrl,
    importCreatedKeyToCcs,
    openUseKeyModal,
    importToCcs,
  }
}
