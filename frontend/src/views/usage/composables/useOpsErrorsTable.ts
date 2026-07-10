// @env browser
import { watchDebounced } from '@vueuse/core'
import { clamp } from 'es-toolkit'
import { computed, onMounted, ref, shallowRef, watch, type Ref } from 'vue'

import { getOpsErrors } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { withMinimumDuration } from '@/utils/async'

export function useOpsErrorsTable(timeRangeParams: Ref<Record<string, string>>) {
  const loading = shallowRef(true)
  const refreshing = shallowRef(false)
  const records = ref<any[]>([])
  const page = shallowRef(1)
  const pageSize = shallowRef(20)
  const total = shallowRef(0)
  const searchQuery = shallowRef('')
  const failureClass = shallowRef('')
  const route = shallowRef('')

  const pagination = computed(() => ({
    page: page.value,
    pageSize: pageSize.value,
    total: total.value,
    pageSizes: [20, 50, 100],
  }))

  async function load() {
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
      records.value = result.items
      pageSize.value = result.page.pageSize ?? pageSize.value
      total.value = result.page.total ?? result.items.length
      page.value = result.page.page ?? page.value

      if (records.value.length === 0 && total.value > 0 && page.value > 1) {
        page.value = clamp(result.page.totalPages ?? page.value - 1, 1, Number.POSITIVE_INFINITY)
        await load()
      }
    } catch (error: any) {
      toast.error(error.message || '加载错误明细失败')
    } finally {
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
    if (refreshing.value || loading.value) return
    refreshing.value = true
    try {
      await withMinimumDuration(load)
    } finally {
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
