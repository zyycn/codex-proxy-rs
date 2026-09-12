import { watchDebounced } from '@vueuse/core'
import { computed, shallowRef, watch } from 'vue'
import { getProxyAccounts, removeProxyAccount } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { usePagedQuery } from '@/composables/usePagedQuery'

export function useProxyAccounts(options: {
  isOpen: () => boolean
  proxyId: () => string | undefined
  onRemoved: () => void
}) {
  const search = shallowRef('')
  const removeAction = useAsyncAction()
  const query = usePagedQuery({
    initialPageSize: 20,
    load: (pagination, requestOptions) => getProxyAccounts({
      ...pagination,
      proxyId: options.proxyId() ?? '',
      search: search.value.trim() || undefined,
    }, requestOptions),
  })
  const pagination = computed(() => ({
    currentPage: query.page.value,
    pageSize: query.pageSize.value,
    total: query.total.value,
  }))

  function load() {
    if (options.isOpen() && options.proxyId())
      void query.execute()
  }

  function setPage(page: number) {
    query.page.value = page
    query.items.value = []
    load()
  }

  function setPageSize(size: number) {
    query.pageSize.value = size
    setPage(1)
  }

  async function removeAccount(accountId: string) {
    const proxyId = options.proxyId()
    if (!proxyId)
      return false
    return await removeAction.run(async () => {
      await removeProxyAccount({ proxyId, accountId })
      options.onRemoved()
      toast.success('账号已移出当前代理，改为直连')
      if (options.isOpen() && options.proxyId() === proxyId) {
        query.invalidate()
        query.items.value = query.items.value.filter(account => account.id !== accountId)
        query.total.value = Math.max(0, query.total.value - 1)
        // 复用分页查询，删除末页最后一项时自动回到有效页。
        await query.execute()
      }
      return true
    }) ?? false
  }

  watch([options.isOpen, options.proxyId], () => {
    // 关闭或切换代理后丢弃旧请求，避免上一条代理的账号覆盖新列表。
    query.invalidate()
    query.items.value = []
    query.total.value = 0
    query.error.value = ''
    query.page.value = 1
    search.value = ''
    load()
  }, { immediate: true })
  watchDebounced(search, () => setPage(1), { debounce: 300 })

  return {
    accounts: query.items,
    loading: query.loading,
    error: query.error,
    pagination,
    search,
    setPage,
    setPageSize,
    removing: removeAction.loading,
    removeAccount,
  }
}
