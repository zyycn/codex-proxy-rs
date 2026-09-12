import type { UsageRecordDetail } from '@/api'
import { shallowRef, watch } from 'vue'
import { getUsageRecordDetail } from '@/api'
import { errorMessage } from '@/utils/async'

/** Requests can be switched while a lookup is pending; stale results never replace the active trace. */
export function useRequestDiagnostics(requestId: () => string) {
  const selectedId = shallowRef(requestId())
  const revision = shallowRef(0)
  const detail = shallowRef<UsageRecordDetail | null>(null)
  const loading = shallowRef(false)
  const error = shallowRef('')

  watch(requestId, id => selectedId.value = id)
  watch([selectedId, revision], async ([id], _previous, onCleanup) => {
    let active = true
    const controller = new AbortController()
    onCleanup(() => {
      active = false
      controller.abort()
    })
    detail.value = null
    error.value = ''
    loading.value = true
    try {
      const result = await getUsageRecordDetail({ id }, { signal: controller.signal })
      if (active)
        detail.value = result
    }
    catch (cause: unknown) {
      if (active)
        error.value = errorMessage(cause)
    }
    finally {
      if (active)
        loading.value = false
    }
  }, { immediate: true })

  return { selectedId, detail, loading, error, refresh: () => revision.value++ }
}
