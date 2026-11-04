import type { OutboundProxyRecord } from '@/api'
import { computed, shallowRef, watch } from 'vue'
import { batchUpdateAccounts, getAccounts } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { usePageSelection } from '@/composables/usePageSelection'
import { t } from '@/i18n'
import { notification as ZNotification } from '@/utils/notification'

const MAX_ASSIGNMENT_ACCOUNTS = 1000

export function useProxyAccountAssignment(options: {
  isOpen: () => boolean
  proxy: () => OutboundProxyRecord | null
  onAssigned: () => void
}) {
  const search = shallowRef('')
  const action = useAsyncAction()
  const query = usePagedQuery({
    initialPageSize: 20,
    load: (pagination, requestOptions) => getAccounts({
      ...pagination,
      search: search.value.trim() || undefined,
    }, requestOptions),
  })
  const selectableAccounts = computed(() => query.items.value.filter(account => account.outboundProxyId !== options.proxy()?.id))
  const selection = usePageSelection(selectableAccounts)
  const tooManySelected = computed(() => selection.selectedIds.value.size > MAX_ASSIGNMENT_ACCOUNTS)
  const pagination = computed(() => ({
    currentPage: query.page.value,
    pageSize: query.pageSize.value,
    total: query.total.value,
  }))

  function setPage(page: number) {
    if (options.isOpen() && !action.loading.value)
      void query.execute({ page })
  }

  function setPageSize(pageSize: number) {
    if (options.isOpen() && !action.loading.value)
      void query.execute({ page: 1, pageSize })
  }

  async function assign() {
    const proxy = options.proxy()
    const accountIds = [...selection.selectedIds.value]
    if (!options.isOpen() || !proxy?.enabled || !accountIds.length || tooManySelected.value)
      return false
    return await action.run(async () => {
      // 只写代理字段，保留账号的凭据、分组与调度设置
      const result = await batchUpdateAccounts({ accountIds, outboundProxyId: proxy.id })
      options.onAssigned()
      ZNotification.success({
        message: t('proxies.assignment.assigned', { count: result.accountIds.length, name: proxy.name }),
      })
      return options.isOpen() && options.proxy()?.id === proxy.id
    }) ?? false
  }

  watch([
    options.isOpen,
    () => options.proxy()?.id,
  ], () => {
    query.invalidate({
      resetLoading: options.isOpen(),
    })
    selection.selectedIds.value = new Set()
    if (!options.isOpen())
      return
    search.value = ''
    if (options.proxy())
      void query.reloadFromStart()
  }, { immediate: true, flush: 'sync' })

  watch(search, (_value, _previous, onCleanup) => {
    const timer = setTimeout(() => {
      if (options.isOpen() && options.proxy())
        void query.reloadFromStart()
    }, 300)
    onCleanup(() => clearTimeout(timer))
  })

  return {
    accounts: query.items,
    loading: query.loading,
    error: query.error,
    search,
    pagination,
    setPage,
    setPageSize,
    ...selection,
    tooManySelected,
    maximumAccounts: MAX_ASSIGNMENT_ACCOUNTS,
    assigning: action.loading,
    assign,
  }
}
