import type { Account, AccountQuotaWindow } from '@/api'
import { shallowRef, watch } from 'vue'
import { getAccountDetail } from '@/api'
import { useRequestState } from '@/composables/useRequestState'

export function usePoolQuotaWindows(account: () => Account) {
  const windows = shallowRef<AccountQuotaWindow[]>([])
  const request = useRequestState()

  watch([
    () => account().id,
    () => account().upstream?.discovered,
    () => account().upstream?.connectionRevision,
    () => account().upstream?.sync.completedGeneration,
    () => account().upstream?.observedAt,
    () => account().upstream?.stale,
  ], async () => {
    request.invalidate()
    const current = account()
    windows.value = current.quota.windows
    if (!current.upstream?.discovered
      || windows.value.some(window => window.sourceAccountId)) {
      return
    }

    // 节点窗口仅在展开面板后读取，列表只携带预算窗口与池汇总
    const requestId = request.start()
    try {
      const detail = await getAccountDetail({ accountId: current.id }, { signal: request.signal })
      if (request.isCurrent(requestId)
        && detail.account.upstream?.connectionRevision === current.upstream.connectionRevision) {
        windows.value = detail.account.quota.windows
      }
    }
    catch (error) {
      request.fail(requestId, error)
    }
    finally {
      request.finish(requestId)
    }
  }, { immediate: true })

  return windows
}
