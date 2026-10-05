import type { BaseTableSort } from '@codex-proxy/ui'
import type { ApiKey } from '@/api'

import { computed, onMounted, shallowRef, watch } from 'vue'
import { getApiKeys } from '@/api'
import { useRequestState } from '@/composables/useRequestState'

export function useApiKeysQuery() {
  const searchQuery = shallowRef('')
  const sort = shallowRef<BaseTableSort>()
  const page = shallowRef(1)
  const pageSize = shallowRef(20)
  const total = shallowRef(0)
  const apiKeys = shallowRef<ApiKey[]>([])
  const request = useRequestState()
  const { loading } = request
  const cursors = new Map<number, string | undefined>([[1, undefined]])

  const apiKeyPagination = computed(() => ({
    currentPage: page.value,
    pageSize: pageSize.value,
    total: total.value,
  }))

  function resetCursorPagination() {
    cursors.clear()
    cursors.set(1, undefined)
    page.value = 1
  }

  function knownPageBefore(targetPage: number) {
    return [...cursors.keys()].reduce(
      (known, candidate) => (candidate <= targetPage && candidate > known ? candidate : known),
      1,
    )
  }

  async function fetchPage(cursor: string | undefined, limit: number, search: string | undefined, signal?: AbortSignal) {
    return getApiKeys({
      cursor,
      limit,
      search,
      sortBy: sort.value?.key,
      sortDirection: sort.value?.direction,
    }, { signal })
  }

  function applyPage(result: Awaited<ReturnType<typeof getApiKeys>>, targetPage: number) {
    apiKeys.value = result.items
    total.value = result.total
    page.value = targetPage
  }

  async function execute(targetPage = page.value) {
    const requestId = request.start()
    const signal = request.signal
    const requestedPage = Math.max(1, targetPage)
    const requestedPageSize = pageSize.value
    const requestedSearch = searchQuery.value.trim() || undefined

    try {
      let currentPage = knownPageBefore(requestedPage)
      let result

      while (currentPage < requestedPage) {
        result = await fetchPage(cursors.get(currentPage), requestedPageSize, requestedSearch, signal)
        if (!request.isCurrent(requestId))
          return false

        if (!result.nextCursor) {
          applyPage(result, currentPage)
          return true
        }

        cursors.set(currentPage + 1, result.nextCursor)
        currentPage += 1
      }

      result = await fetchPage(cursors.get(currentPage), requestedPageSize, requestedSearch, signal)
      if (!request.isCurrent(requestId))
        return false

      if (result.nextCursor)
        cursors.set(currentPage + 1, result.nextCursor)
      else
        cursors.delete(currentPage + 1)
      applyPage(result, currentPage)
      return true
    }
    catch {
      return false
    }
    finally {
      request.finish(requestId)
    }
  }

  async function reloadFromStart() {
    resetCursorPagination()
    return execute(1)
  }

  function handlePageChange(page: number) {
    void execute(page)
  }

  function handlePageSizeChange(nextPageSize: number) {
    pageSize.value = nextPageSize
    void reloadFromStart()
  }

  function handleSortChange(nextSort: BaseTableSort | undefined) {
    sort.value = nextSort
    void reloadFromStart()
  }

  watch(searchQuery, (_value, _previous, onCleanup) => {
    const timer = setTimeout(() => {
      void reloadFromStart()
    }, 250)
    onCleanup(() => clearTimeout(timer))
  })

  onMounted(() => {
    void execute()
  })

  return {
    loading,
    apiKeys,
    loadApiKeys: reloadFromStart,
    searchQuery,
    sort,
    apiKeyPagination,
    handlePageChange,
    handlePageSizeChange,
    handleSortChange,
  }
}
