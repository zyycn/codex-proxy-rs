import { computed, onMounted, ref, type Ref } from 'vue'

import { clearLogs, getLogs } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { withMinimumDuration } from '@/utils/async'

export function useLogsTable(options: {
  page: Ref<number>
  pageSize: Ref<number>
  searchQuery: Ref<string>
  filterLevel: Ref<string>
  totalLogs: Ref<number>
}) {
  const loading = ref(true)
  const logs = ref<any[]>([])
  const showClearModal = ref(false)
  const refreshingList = ref(false)
  const clearingLogs = ref(false)
  const loaded = ref(false)

  const initialLoading = computed(() => loading.value && !loaded.value)

  async function loadLogs() {
    try {
      loading.value = true
      const result = await getLogs({
        page: options.page.value,
        pageSize: options.pageSize.value,
        level: options.filterLevel.value || undefined,
        search: options.searchQuery.value || undefined,
      })
      logs.value = result.items
      options.pageSize.value = result.page.pageSize ?? options.pageSize.value
      options.totalLogs.value = result.page.total ?? result.items.length
      options.page.value = result.page.page ?? options.page.value

      if (logs.value.length === 0 && options.totalLogs.value > 0 && options.page.value > 1) {
        options.page.value = Math.max(1, result.page.totalPages ?? options.page.value - 1)
        await loadLogs()
      }
    } catch (error: any) {
      toast.error(error.message || '加载失败')
    } finally {
      loading.value = false
      loaded.value = true
    }
  }

  async function refreshLogs() {
    if (refreshingList.value || loading.value) return
    refreshingList.value = true
    try {
      await withMinimumDuration(loadLogs)
    } finally {
      refreshingList.value = false
    }
  }

  async function handleClearLogs() {
    if (clearingLogs.value) return

    try {
      clearingLogs.value = true
      await clearLogs()
      showClearModal.value = false
      options.page.value = 1
      await loadLogs()
      toast.success('日志已清空')
    } catch (error: any) {
      toast.error(error.message || '清空失败')
    } finally {
      clearingLogs.value = false
    }
  }

  onMounted(() => {
    loadLogs()
  })

  return {
    loading,
    logs,
    showClearModal,
    refreshingList,
    clearingLogs,
    initialLoading,
    loadLogs,
    refreshLogs,
    handleClearLogs,
  }
}
