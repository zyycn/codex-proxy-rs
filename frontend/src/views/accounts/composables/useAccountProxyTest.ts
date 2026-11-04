import type { OutboundProxyRecord, OutboundProxyTest } from '@/api'
import { computed, shallowRef, watch } from 'vue'
import { probeProxy, testProxy } from '@/api'
import { useRequestState } from '@/composables/useRequestState'
import { t } from '@/i18n'

export function useAccountProxyTest(options: {
  target: () => OutboundProxyRecord | 'direct' | null
  accountId: () => string | undefined
  onTested: (proxy: OutboundProxyRecord) => void
}) {
  const request = useRequestState()
  const result = shallowRef<OutboundProxyTest | null>(null)
  const targetId = computed(() => {
    const target = options.target()
    return typeof target === 'object'
      ? target?.id
      : target
  })

  watch([targetId, options.accountId], () => {
    request.invalidate()
    result.value = null
    request.error.value = ''
  }, { flush: 'sync' })

  async function test() {
    const target = options.target()
    if (!target || request.loading.value)
      return
    const requestId = request.start()
    result.value = null
    const requestOptions = { signal: request.signal, silent: true }
    try {
      if (target === 'direct') {
        const response = await probeProxy({ proxyUrl: '', detectLocation: false }, requestOptions)
        if (request.isCurrent(requestId))
          result.value = response
      }
      else {
        const [response] = await testProxy([
          { id: target.id, revision: target.revision, detectLocation: false },
        ], requestOptions)
        if (!request.isCurrent(requestId))
          return
        if (response?.status !== 'succeeded') {
          throw new Error(response?.status === 'failed'
            ? response.message
            : t('accounts.proxy.testIncomplete'))
        }
        options.onTested(response.data)
        result.value = response.data.lastTest
      }
    }
    catch (error) {
      request.fail(requestId, error)
    }
    finally {
      request.finish(requestId)
    }
  }

  return {
    result,
    testing: request.loading,
    error: request.error,
    test,
  }
}
