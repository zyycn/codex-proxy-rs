import { onScopeDispose, shallowRef } from 'vue'

import { errorMessage } from '@/utils/async'

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

/** Element Plus 风格的页面分页状态；稳定深分页由 API pager 内部完成。 */
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
  const loading = shallowRef(false)
  const error = shallowRef('')
  let requestSequence = 0

  async function execute(targetPage = currentPage.value, execution: { silent?: boolean } = {}) {
    const requestId = ++requestSequence
    if (!execution.silent) {
      loading.value = true
      error.value = ''
    }

    try {
      const result = await options.load({
        currentPage: Math.max(1, targetPage),
        pageSize: pageSize.value,
      })
      if (requestId !== requestSequence)
        return false

      items.value = result.items
      currentPage.value = result.currentPage
      pageSize.value = result.pageSize
      total.value = result.total
      options.onSuccess?.(result)
      return true
    }
    catch (cause: unknown) {
      if (requestId !== requestSequence)
        return false
      if (!execution.silent) {
        error.value = errorMessage(cause, '加载失败')
        options.onError?.(cause)
      }
      return false
    }
    finally {
      if (requestId === requestSequence)
        loading.value = false
    }
  }

  function reloadFromStart(execution: { silent?: boolean } = {}) {
    currentPage.value = 1
    total.value = 0
    return execute(1, execution)
  }

  function invalidate() {
    requestSequence += 1
    loading.value = false
  }

  onScopeDispose(invalidate)

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
