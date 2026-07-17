import { clamp } from 'es-toolkit'
import { onScopeDispose, shallowRef } from 'vue'

import { errorMessage } from '@/utils/async'

interface PageResult {
  items: unknown[]
  page: {
    page: number
    pageSize: number
    total: number
    totalPages: number
  }
}

export function usePagedQuery<Result extends PageResult>(options: {
  initialPageSize: number
  load: (pagination: { page: number, pageSize: number }) => Promise<Result>
  onSuccess?: (result: Result) => void
  onError?: (error: unknown) => void
}) {
  const page = shallowRef(1)
  const pageSize = shallowRef(options.initialPageSize)
  const total = shallowRef(0)
  const items = shallowRef<Result['items'][number][]>([])
  const loading = shallowRef(false)
  const error = shallowRef('')
  let requestSequence = 0

  async function execute() {
    const requestId = ++requestSequence
    loading.value = true
    error.value = ''

    try {
      const result = await options.load({
        page: page.value,
        pageSize: pageSize.value,
      })
      if (requestId !== requestSequence)
        return false

      if (result.items.length === 0 && result.page.total > 0 && result.page.page > 1) {
        page.value = clamp(result.page.totalPages, 1, Number.POSITIVE_INFINITY)
        return execute()
      }

      items.value = result.items
      page.value = result.page.page
      pageSize.value = result.page.pageSize
      total.value = result.page.total
      options.onSuccess?.(result)
      return true
    }
    catch (cause: unknown) {
      if (requestId !== requestSequence)
        return false
      error.value = errorMessage(cause, '加载失败')
      options.onError?.(cause)
      return false
    }
    finally {
      if (requestId === requestSequence)
        loading.value = false
    }
  }

  function invalidate() {
    requestSequence += 1
    loading.value = false
  }

  onScopeDispose(invalidate)

  return {
    page,
    pageSize,
    total,
    items,
    loading,
    error,
    execute,
    invalidate,
  }
}
