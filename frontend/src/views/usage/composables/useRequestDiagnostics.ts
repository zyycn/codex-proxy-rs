import type { UsageRecordDetail } from '@/api'
import { shallowRef, watch } from 'vue'
import { getUsageRecordDetail } from '@/api'
import { errorMessage } from '@/utils/operation'

// 切换请求或开始退场时取消旧查询，退场期间保留最后一次诊断画面。
export function useRequestDiagnostics(requestId: () => string, isActive: () => boolean = () => true) {
  const selectedId = shallowRef(requestId())
  const revision = shallowRef(0)
  const detail = shallowRef<UsageRecordDetail | null>(null)
  const loading = shallowRef(false)
  const error = shallowRef('')

  watch(requestId, id => selectedId.value = id)
  watch([selectedId, revision, isActive], async ([id, , enabled], _previous, onCleanup) => {
    if (!enabled)
      return
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
