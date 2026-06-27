import { computed, ref, watch, type Ref } from 'vue'

export function useApiKeyFilters(apiKeys: Ref<any[]>) {
  const page = ref(1)
  const pageSize = ref(20)
  const searchQuery = ref('')

  const filteredKeys = computed(() => {
    if (!searchQuery.value) return apiKeys.value
    const query = searchQuery.value.toLowerCase()
    return apiKeys.value.filter(
      (key) =>
        key.name.toLowerCase().includes(query) ||
        key.label?.toLowerCase().includes(query) ||
        key.id.toLowerCase().includes(query),
    )
  })

  const pagedKeys = computed(() => {
    const start = (page.value - 1) * pageSize.value
    return filteredKeys.value.slice(start, start + pageSize.value)
  })

  const apiKeyPagination = computed(() => ({
    page: page.value,
    pageSize: pageSize.value,
    total: filteredKeys.value.length,
    pageSizes: [10, 20, 50, 100],
  }))

  function handlePageChange(nextPage: number) {
    page.value = nextPage
  }

  function handlePageSizeChange(nextPageSize: number) {
    pageSize.value = nextPageSize
    page.value = 1
  }

  watch(searchQuery, () => {
    page.value = 1
  })

  watch(filteredKeys, () => {
    const totalPages = Math.max(1, Math.ceil(filteredKeys.value.length / pageSize.value))
    if (page.value > totalPages) {
      page.value = totalPages
    }
  })

  return {
    page,
    pageSize,
    searchQuery,
    pagedKeys,
    apiKeyPagination,
    handlePageChange,
    handlePageSizeChange,
  }
}
