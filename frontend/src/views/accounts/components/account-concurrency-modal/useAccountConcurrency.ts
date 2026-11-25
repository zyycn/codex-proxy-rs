import type { Ref } from 'vue'
import type { Account, AccountConcurrencyPage } from '@/api'
import { useDocumentVisibility, useIntervalFn } from '@vueuse/core'
import { computed, onScopeDispose, shallowRef, watch } from 'vue'
import { getAccountConcurrency } from '@/api'
import { useRequestState } from '@/composables/useRequestState'

export function useAccountConcurrency(account: Ref<Account | null>, open: Ref<boolean>) {
  const data = shallowRef<Pick<AccountConcurrencyPage, 'entries'> | null>(null)
  const receivedAt = shallowRef(0)
  const now = shallowRef(performance.now())
  const visibility = useDocumentVisibility()
  const active = computed(() => open.value && !!account.value && visibility.value === 'visible')
  const request = useRequestState()
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
      const accountId = account.value.id
      const loadPage = (page: number) => getAccountConcurrency({ accountId, page, pageSize: 100 }, { signal: request.signal, silent: true, retry: false })
      const result = await loadPage(1)
      if (!request.isCurrent(id))
        return
      const firstReceivedAt = performance.now()
      const collected = new Map(result.entries.map(entry => [
        entry.leaseId,
        { entry, receivedAt: firstReceivedAt },
      ]))
      // 按首次总数限定本轮读取，避免持续新增占用让刷新无界延长
      const pages = Math.ceil(result.total / result.pageSize)
      for (let page = 2; page <= pages; page++) {
        const next = await loadPage(page)
        if (!request.isCurrent(id))
          return
        const receivedAt = performance.now()
        for (const entry of next.entries)
          collected.set(entry.leaseId, { entry, receivedAt })
        if (!next.entries.length)
          break
      }
      const snapshotAt = performance.now()
      data.value = {
        entries: [
          ...collected.values(),
        ].map(({ entry, receivedAt }) => ({
          ...entry,
          occupiedMs: entry.occupiedMs === null
            ? null
            : entry.occupiedMs + snapshotAt - receivedAt,
        })),
      }
      request.error.value = ''
      receivedAt.value = snapshotAt
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

  watch([
    active,
    () => account.value?.id,
  ], ([isActive, accountId], [, previousAccountId]) => {
    stop()
    // 同一账号保留已确认内容，暂停轮询和弹窗退场不提前清空
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

  return {
    data,
    entries,
    loading: request.loading,
    error: request.error,
  }
}
