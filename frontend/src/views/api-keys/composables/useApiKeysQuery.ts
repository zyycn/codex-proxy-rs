import type { BaseTableSort } from '@/components/base/BaseTable/columns'
import { watchDebounced } from '@vueuse/core'

import { computed, onMounted, onScopeDispose, shallowRef } from 'vue'
import { getApiKeys } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { errorMessage } from '@/utils/async'
import { formatDateTime } from '@/utils/date'

export function useApiKeysQuery() {
  const searchQuery = shallowRef('')
  const sort = shallowRef<BaseTableSort>()
  const page = shallowRef(1)
  const pageSize = shallowRef(20)
  const total = shallowRef(0)
  const apiKeys = shallowRef<
    Array<Awaited<ReturnType<typeof getApiKeys>>['items'][number]>
  >([])
  const loading = shallowRef(false)
  const configRevision = shallowRef(0)
  const cursors = new Map<number, string | undefined>([[1, undefined]])
  let requestSequence = 0

  const apiKeyPagination = computed(() => ({
    page: page.value,
    pageSize: pageSize.value,
    total: total.value,
    pageSizes: [10, 20, 50, 100],
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

  async function fetchPage(cursor: string | undefined, limit: number, search: string | undefined) {
    return getApiKeys({
      cursor,
      limit,
      search,
      sortBy: sort.value?.key,
      sortDirection: sort.value?.direction,
    })
  }

  function applyPage(result: Awaited<ReturnType<typeof getApiKeys>>, targetPage: number) {
    apiKeys.value = result.items.map((item: (typeof result.items)[number]) => ({
      ...item,
      createdAtDisplay: formatDateTime(item.createdAt),
      lastUsedAtDisplay: item.lastUsedAt ? formatDateTime(item.lastUsedAt) : '—',
    }))
    configRevision.value = result.configRevision
    total.value = result.total
    page.value = targetPage
  }

  function paginationRevisionChanged(result: Awaited<ReturnType<typeof getApiKeys>>) {
    return configRevision.value > 0 && result.configRevision !== configRevision.value
  }

  async function execute(targetPage = page.value) {
    const requestId = ++requestSequence
    const requestedPage = Math.max(1, targetPage)
    const requestedPageSize = pageSize.value
    const requestedSearch = searchQuery.value.trim() || undefined
    loading.value = true

    try {
      let currentPage = knownPageBefore(requestedPage)
      let result

      while (currentPage < requestedPage) {
        result = await fetchPage(cursors.get(currentPage), requestedPageSize, requestedSearch)
        if (requestId !== requestSequence)
          return false
        if (paginationRevisionChanged(result)) {
          resetCursorPagination()
          return execute(1)
        }

        if (!result.nextCursor) {
          applyPage(result, currentPage)
          return true
        }

        cursors.set(currentPage + 1, result.nextCursor)
        currentPage += 1
      }

      result = await fetchPage(cursors.get(currentPage), requestedPageSize, requestedSearch)
      if (requestId !== requestSequence)
        return false
      if (paginationRevisionChanged(result)) {
        resetCursorPagination()
        if (currentPage !== 1)
          return execute(1)
      }

      if (result.nextCursor)
        cursors.set(currentPage + 1, result.nextCursor)
      else
        cursors.delete(currentPage + 1)
      applyPage(result, currentPage)
      return true
    }
    catch (error: unknown) {
      if (requestId === requestSequence)
        toast.error(errorMessage(error, 'API Key 加载失败'))
      return false
    }
    finally {
      if (requestId === requestSequence)
        loading.value = false
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

  watchDebounced(
    searchQuery,
    () => {
      void reloadFromStart()
    },
    { debounce: 250 },
  )

  onMounted(() => {
    void execute()
  })
  onScopeDispose(() => {
    requestSequence += 1
  })

  return {
    loading,
    apiKeys,
    loadApiKeys: reloadFromStart,
    configRevision,
    searchQuery,
    sort,
    apiKeyPagination,
    handlePageChange,
    handlePageSizeChange,
    handleSortChange,
  }
}
