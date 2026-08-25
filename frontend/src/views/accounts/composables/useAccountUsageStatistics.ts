import type { Ref } from 'vue'
import { shallowRef, watch } from 'vue'

import { getAccountUsageStatistics } from '@/api'
import { errorMessage } from '@/utils/async'

export function useAccountUsageStatistics(accountId: Ref<string>, open: Ref<boolean>) {
  const view = shallowRef<Awaited<ReturnType<typeof getAccountUsageStatistics>> | null>(null)
  const loadedAccountId = shallowRef('')
  const loading = shallowRef(false)
  const error = shallowRef('')
  let requestVersion = 0

  async function loadCycle(cycleOffset: number, force = false) {
    const targetAccountId = accountId.value
    if (!targetAccountId)
      return
    if (
      !force
      && view.value
      && loadedAccountId.value === targetAccountId
      && view.value.cycle.offset === cycleOffset
    ) {
      return
    }

    const version = ++requestVersion
    loading.value = true
    error.value = ''
    try {
      const result = await getAccountUsageStatistics({
        accountId: targetAccountId,
        cycleOffset,
        utcOffsetMinutes: -new Date().getTimezoneOffset(),
      })
      if (version !== requestVersion)
        return
      view.value = result
      loadedAccountId.value = targetAccountId
    }
    catch (cause) {
      if (version !== requestVersion)
        return
      error.value = errorMessage(cause, '官方用量统计加载失败')
    }
    finally {
      if (version === requestVersion)
        loading.value = false
    }
  }

  function load(force = false) {
    return loadCycle(view.value?.cycle.offset ?? 0, force)
  }

  function previousCycle() {
    const cycle = view.value?.cycle
    if (cycle?.canGoPrevious)
      void loadCycle(cycle.offset - 1)
  }

  function nextCycle() {
    const cycle = view.value?.cycle
    if (cycle?.canGoNext)
      void loadCycle(cycle.offset + 1)
  }

  watch([open, accountId], ([isOpen, nextAccountId], previousValues) => {
    const previousAccountId = previousValues?.[1]
    if (nextAccountId !== previousAccountId) {
      requestVersion += 1
      view.value = null
      loadedAccountId.value = ''
      loading.value = false
      error.value = ''
    }
    if (isOpen)
      void load()
  }, { immediate: true })

  return {
    loading,
    error,
    view,
    load,
    previousCycle,
    nextCycle,
  }
}
