import { shallowRef } from 'vue'

import { useRequestState } from './useRequestState'

interface PageResult {
  items: unknown[]
  currentPage: number
  pageSize: number
  total: number
}

interface PageRequest {
  currentPage: number
  pageSize: number
}

export function useStablePagedQuery<Result extends PageResult>(options: {
  initialPageSize: number
  load: (pagination: PageRequest) => Promise<Result>
  onSuccess?: (result: Result) => void
  onError?: (error: unknown) => void
}) {
  const currentPage = shallowRef(1)
  const pageSize = shallowRef(options.initialPageSize)
  const total = shallowRef(0)
  const items = shallowRef<Result['items'][number][]>([])
  const request = useRequestState(options.onError)
  const { loading, error, invalidate } = request

  async function execute(targetPage = currentPage.value, execution: { silent?: boolean } = {}) {
    const requestId = request.start(execution.silent)

    try {
      const result = await options.load({
        currentPage: Math.max(1, targetPage),
        pageSize: pageSize.value,
      })
      if (!request.isCurrent(requestId))
        return false

      items.value = result.items
      currentPage.value = result.currentPage
      pageSize.value = result.pageSize
      total.value = result.total
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

  function reloadFromStart(execution: { silent?: boolean } = {}) {
    currentPage.value = 1
    total.value = 0
    return execute(1, execution)
  }

  return {
    currentPage,
    pageSize,
    total,
    items,
    loading,
    error,
    execute,
    reloadFromStart,
    invalidate,
  }
}
