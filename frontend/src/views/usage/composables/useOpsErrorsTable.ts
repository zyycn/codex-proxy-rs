import type { Ref } from 'vue'
import type { UsageTimeRangeParams } from './useUsageTimeRange'
import { watchDebounced } from '@vueuse/core'
import { clamp } from 'es-toolkit'

import { computed, onMounted, shallowRef, watch } from 'vue'
import { getOpsErrors } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { errorMessage, withMinimumDuration } from '@/utils/async'

export function useOpsErrorsTable(timeRangeParams: Readonly<Ref<UsageTimeRangeParams>>) {
  const loading = shallowRef(true)
  const refreshing = shallowRef(false)
  const records = shallowRef<Awaited<ReturnType<typeof getOpsErrors>>['items']>([])
  const page = shallowRef(1)
  const pageSize = shallowRef(10)
  const total = shallowRef(0)
  const searchQuery = shallowRef('')
  const failureClass = shallowRef('')
  const route = shallowRef('')
  let loadRequestId = 0

  const pagination = computed(() => ({
    page: page.value,
    pageSize: pageSize.value,
    total: total.value,
    pageSizes: [10, 20, 50, 100],
  }))

  async function load() {
    const requestId = ++loadRequestId
    try {
      loading.value = true
      const result = await getOpsErrors({
        page: page.value,
        pageSize: pageSize.value,
        search: searchQuery.value.trim() || undefined,
        failureClass: failureClass.value.trim() || undefined,
        route: route.value.trim() || undefined,
        ...timeRangeParams.value,
      })
      if (requestId !== loadRequestId)
        return
      records.value = result.items
      pageSize.value = result.page.pageSize
      total.value = result.page.total
      page.value = result.page.page

      if (records.value.length === 0 && total.value > 0 && page.value > 1) {
        page.value = clamp(result.page.totalPages, 1, Number.POSITIVE_INFINITY)
        await load()
      }
    }
    catch (error: unknown) {
      if (requestId !== loadRequestId)
        return
      toast.error(errorMessage(error, '加载错误明细失败'))
    }
    finally {
      if (requestId === loadRequestId)
        loading.value = false
    }
  }

  function handlePageChange(nextPage: number) {
    page.value = nextPage
    void load()
  }

  function handlePageSizeChange(nextPageSize: number) {
    pageSize.value = nextPageSize
    page.value = 1
    void load()
  }

  async function refresh() {
    if (refreshing.value || loading.value)
      return
    refreshing.value = true
    try {
      await withMinimumDuration(load)
    }
    finally {
      refreshing.value = false
    }
  }

  watchDebounced(
    [searchQuery, failureClass, route],
    () => {
      page.value = 1
      void load()
    },
    { debounce: 250 },
  )

  watch(timeRangeParams, () => {
    page.value = 1
    void load()
  })

  onMounted(load)

  return {
    loading,
    refreshing,
    records,
    searchQuery,
    failureClass,
    route,
    pagination,
    handlePageChange,
    handlePageSizeChange,
    refresh,
  }
}
