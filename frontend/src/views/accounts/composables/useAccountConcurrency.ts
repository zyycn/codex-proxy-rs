import type { Ref } from 'vue'
import type { Account, AccountConcurrencyPage } from '@/api'
import { useDocumentVisibility, useIntervalFn } from '@vueuse/core'
import { computed, onScopeDispose, shallowRef, watch } from 'vue'
import { getAccountConcurrency } from '@/api'
import { useRequestState } from '@/composables/useRequestState'

export function useAccountConcurrency(account: Ref<Account | null>, open: Ref<boolean>) {
  const page = shallowRef(1)
  const pageSize = shallowRef(20)
  const data = shallowRef<AccountConcurrencyPage | null>(null)
  const receivedAt = shallowRef(0)
  const now = shallowRef(performance.now())
  const visibility = useDocumentVisibility()
  const active = computed(() => open.value && !!account.value && visibility.value === 'visible')
  const request = useRequestState()
  const tableLoading = computed(() => request.loading.value && (
    !data.value || data.value.page !== page.value || data.value.pageSize !== pageSize.value
  ))
  let timer: ReturnType<typeof setTimeout> | undefined

  const ticker = useIntervalFn(() => {
    if (!request.error.value)
      now.value = performance.now()
  }, 1000, { immediate: false })
  const entries = computed(() => (data.value?.entries ?? []).map(entry => ({
    ...entry,
    occupiedMs: entry.occupiedMs === null
      ? null
      : entry.occupiedMs + Math.max(0, now.value - receivedAt.value),
  })))

  function stop() {
    clearTimeout(timer)
    request.invalidate()
    ticker.pause()
  }

  async function refresh() {
    clearTimeout(timer)
    if (!active.value || !account.value)
      return
    // 失败状态与时长保持到新快照确认，重试开始不把旧数据重新当作实时结果
    const id = request.start(true)
    request.loading.value = true
    try {
      const result = await getAccountConcurrency({
        accountId: account.value.id,
        page: page.value,
        pageSize: pageSize.value,
      }, { signal: request.signal, silent: true, retry: false })
      if (!request.isCurrent(id))
        return
      const lastPage = Math.max(1, Math.ceil(result.total / pageSize.value))
      if (page.value > lastPage) {
        page.value = lastPage
        return
      }
      data.value = result
      request.error.value = ''
      receivedAt.value = performance.now()
      now.value = receivedAt.value
    }
    catch (cause: unknown) {
      request.fail(id, cause)
    }
    finally {
      request.finish(id)
      // 每次读取结束后再安排下一次，慢接口不会累积轮询
      if (request.isCurrent(id) && active.value)
        timer = setTimeout(() => void refresh(), 5000)
    }
  }

  watch(() => account.value?.id, () => {
    page.value = 1
  })
  watch(pageSize, () => {
    page.value = 1
  })
  watch([active, () => account.value?.id, page, pageSize], ([isActive, accountId], [, previousAccountId]) => {
    stop()
    // 同一账号保留已确认的页快照，切页、暂停轮询和弹窗退场都不提前清空内容
    if (accountId !== previousAccountId) {
      data.value = null
      request.error.value = ''
    }
    if (isActive) {
      ticker.resume()
      void refresh()
    }
  }, { immediate: true })
  onScopeDispose(stop)

  function setPage(value: number) {
    if (page.value === value)
      void refresh()
    else page.value = value
  }

  function setPageSize(value: number) {
    if (pageSize.value === value)
      void refresh()
    else pageSize.value = value
  }

  return { page, pageSize, data, entries, loading: request.loading, tableLoading, error: request.error, refresh, setPage, setPageSize }
}
