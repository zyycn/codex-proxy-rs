import type { Ref } from 'vue'
import { shallowRef, watch } from 'vue'

import { getAccountProfileStatistics } from '@/api'
import { useRequestState } from '@/composables/useRequestState'

export function useAccountProfileStatistics(accountId: Ref<string>, open: Ref<boolean>) {
  const profile = shallowRef<Awaited<ReturnType<typeof getAccountProfileStatistics>> | null>(null)
  const loadedAccountId = shallowRef('')
  const request = useRequestState()
  const { loading, error } = request

  async function load(force = false) {
    const targetAccountId = accountId.value
    if (!targetAccountId)
      return
    if (!force && profile.value && loadedAccountId.value === targetAccountId)
      return

    const version = request.start()
    try {
      const result = await getAccountProfileStatistics({ accountId: targetAccountId }, { signal: request.signal })
      if (!request.isCurrent(version))
        return
      profile.value = result
      loadedAccountId.value = targetAccountId
    }
    catch (cause) {
      request.fail(version, cause)
    }
    finally {
      request.finish(version)
    }
  }

  watch([open, accountId], ([isOpen, nextAccountId], previousValues) => {
    const previousAccountId = previousValues?.[1]
    if (nextAccountId !== previousAccountId) {
      request.invalidate()
      profile.value = null
      loadedAccountId.value = ''
      loading.value = false
      error.value = ''
    }
    if (isOpen)
      void load()
    else
      request.invalidate()
  }, { immediate: true })

  return {
    profile,
    loading,
    error,
    load,
  }
}
