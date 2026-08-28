import type { Ref } from 'vue'
import { shallowRef, watch } from 'vue'

import { getAccountProfileStatistics } from '@/api'
import { errorMessage } from '@/utils/async'

export function useAccountProfileStatistics(accountId: Ref<string>, open: Ref<boolean>) {
  const profile = shallowRef<Awaited<ReturnType<typeof getAccountProfileStatistics>> | null>(null)
  const loadedAccountId = shallowRef('')
  const loading = shallowRef(false)
  const error = shallowRef('')
  let requestVersion = 0

  async function load(force = false) {
    const targetAccountId = accountId.value
    if (!targetAccountId)
      return
    if (!force && profile.value && loadedAccountId.value === targetAccountId)
      return

    const version = ++requestVersion
    loading.value = true
    error.value = ''
    try {
      const result = await getAccountProfileStatistics({ accountId: targetAccountId })
      if (version !== requestVersion)
        return
      profile.value = result
      loadedAccountId.value = targetAccountId
    }
    catch (cause) {
      if (version !== requestVersion)
        return
      error.value = errorMessage(cause, '官方个人资料加载失败')
    }
    finally {
      if (version === requestVersion)
        loading.value = false
    }
  }

  watch([open, accountId], ([isOpen, nextAccountId], previousValues) => {
    const previousAccountId = previousValues?.[1]
    if (nextAccountId !== previousAccountId) {
      requestVersion += 1
      profile.value = null
      loadedAccountId.value = ''
      loading.value = false
      error.value = ''
    }
    if (isOpen)
      void load()
  }, { immediate: true })

  return {
    profile,
    loading,
    error,
    load,
  }
}
