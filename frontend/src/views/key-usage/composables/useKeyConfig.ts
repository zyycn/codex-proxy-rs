import { computed, onScopeDispose, shallowRef } from 'vue'
import { getKeyUsageConfig } from '@/api'
import { resolveServiceRootUrl } from '@/components/api-key-config-modal/client'
import { useCopyText } from '@/composables/useCopyText'
import { useRequestState } from '@/composables/useRequestState'

export function useKeyConfig() {
  const showConfig = shallowRef(false)
  const configKey = shallowRef<{ name: string, key: string } | null>(null)
  const request = useRequestState()
  const copyText = useCopyText()
  const apiBaseUrl = computed(() => `${resolveServiceRootUrl()}/v1`)

  async function openConfig() {
    if (request.loading.value)
      return
    const requestId = request.start()
    try {
      const config = await getKeyUsageConfig({ signal: request.signal })
      if (request.isCurrent(requestId)) {
        configKey.value = { name: config.name, key: config.plaintextKey }
        showConfig.value = true
      }
    }
    catch (cause) {
      request.fail(requestId, cause)
    }
    finally {
      request.finish(requestId)
    }
  }

  function copyConfig(text: string) {
    return copyText(text, {
      notifyWhenEmpty: true,
    })
  }

  // 明文只随弹窗保存在内存中，不进入登录状态或浏览器持久化存储。
  onScopeDispose(() => {
    configKey.value = null
  })

  return {
    showConfig,
    configKey,
    configuring: request.loading,
    apiBaseUrl,
    openConfig,
    copyConfig,
  }
}
