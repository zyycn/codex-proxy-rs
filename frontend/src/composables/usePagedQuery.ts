import type { RequestOptions } from '@/api/request'
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

export interface PagedQueryExecution {
  page?: number
  pageSize?: number
  silent?: boolean
  background?: boolean
}

export function usePagedQuery<Result extends PageResult>(options: {
  initialPageSize: number
  load: (pagination: { page: number, pageSize: number }, options: RequestOptions) => Promise<Result>
  onSuccess?: (result: Result) => void
  onError?: (error: unknown) => void
}) {
  const page = shallowRef(1)
  const pageSize = shallowRef(options.initialPageSize)
  const total = shallowRef(0)
  const items = shallowRef<Result['items'][number][]>([])
  const request = useRequestState(options.onError)
  const { loading, error } = request
  let confirmedPagination = { page: page.value, pageSize: pageSize.value }

  function restorePagination() {
    page.value = confirmedPagination.page
    pageSize.value = confirmedPagination.pageSize
  }

  function invalidate(options?: Parameters<typeof request.invalidate>[0]) {
    request.invalidate(options)
    restorePagination()
  }

  async function execute(execution: PagedQueryExecution = {}): Promise<boolean> {
    // 保留当前列表的刷新不一定静默；手动刷新仍需由请求层提示失败。
    const background = execution.background || execution.silent
    const requestId = request.start(background)
    const pagination = {
      page: execution.page ?? page.value,
      pageSize: execution.pageSize ?? pageSize.value,
    }
    // 同步控件的目标值，失败时再回退；只在成功时写入会让控件内部模型无法恢复。
    // 轮询或操作后的回读也沿用当前目标页，避免把尚未完成的翻页拉回旧页。
    page.value = pagination.page
    pageSize.value = pagination.pageSize

    try {
      const result = await options.load(pagination, { silent: execution.silent, signal: request.signal })
      if (!request.isCurrent(requestId))
        return false

      if (result.items.length === 0 && result.page.total > 0 && result.page.page > 1) {
        return execute({
          ...execution,
          page: clamp(result.page.totalPages, 1, Number.POSITIVE_INFINITY),
        })
      }

      items.value = result.items
      page.value = result.page.page
      pageSize.value = result.page.pageSize
      total.value = result.page.total
      confirmedPagination = { page: page.value, pageSize: pageSize.value }
      options.onSuccess?.(result)
      return true
    }
    catch (cause: unknown) {
      if (request.isCurrent(requestId))
        restorePagination()
      request.fail(requestId, cause, background)
      return false
    }
    finally {
      request.finish(requestId)
    }
  }

  function reloadFromStart() {
    // 筛选条件已改变，旧结果不能继续对应当前查询，也不能在失败时恢复旧页。
    items.value = []
    total.value = 0
    confirmedPagination = { page: 1, pageSize: confirmedPagination.pageSize }
    return execute(confirmedPagination)
  }

  return {
    page,
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
