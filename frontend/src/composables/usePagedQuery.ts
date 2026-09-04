import { clamp } from 'es-toolkit'
import { shallowRef } from 'vue'

import { useRequestState } from './useRequestState'

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
  const request = useRequestState(options.onError)
  const { loading, error, invalidate } = request

  async function execute(execution: { silent?: boolean } = {}) {
    const requestId = request.start(execution.silent)

    try {
      const result = await options.load({
        page: page.value,
        pageSize: pageSize.value,
      })
      if (!request.isCurrent(requestId))
        return false

      if (result.items.length === 0 && result.page.total > 0 && result.page.page > 1) {
        page.value = clamp(result.page.totalPages, 1, Number.POSITIVE_INFINITY)
        return execute(execution)
      }

      items.value = result.items
      page.value = result.page.page
      pageSize.value = result.page.pageSize
      total.value = result.page.total
      options.onSuccess?.(result)
      return true
    }
    catch (cause: unknown) {
      request.fail(requestId, cause, execution.silent)
      return false
    }
    finally {
      request.finish(requestId)
    }
  }

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
