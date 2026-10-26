import type { Ref } from 'vue'
import type { Account, AccountQuotaForecastResponse } from '@/api'
import { onScopeDispose, shallowRef, watch } from 'vue'
import { getAccountQuotaForecast, refreshAccountQuota } from '@/api'
import { useRequestState } from '@/composables/useRequestState'
import { t } from '@/i18n'
import { notification as ZNotification } from '@/utils/notification'
import { errorMessage } from '@/utils/operation'

export function useAccountQuotaForecast(
  accountId: Ref<string>,
  open: Ref<boolean>,
  onAccountUpdated: (account: Account) => void,
) {
  const report = shallowRef<AccountQuotaForecastResponse | null>(null)
  const request = useRequestState()
  const { loading, error } = request
  const refreshing = shallowRef(false)
  let disposed = false

  async function load() {
    if (!open.value || !accountId.value || disposed)
      return false
    const requestId = request.start()
    try {
      const result = await getAccountQuotaForecast({ accountId: accountId.value }, { signal: request.signal })
      if (request.isCurrent(requestId)) {
        report.value = result
        return true
      }
    }
    catch (cause) {
      request.fail(requestId, cause)
    }
    finally {
      request.finish(requestId)
    }
    return false
  }

  async function refresh() {
    if (refreshing.value || !open.value)
      return
    const targetAccountId = accountId.value
    request.invalidate()
    refreshing.value = true
    error.value = ''
    try {
      // 刷新属于现有额度动作；预测查询本身始终只读。
      const result = await refreshAccountQuota({ accountId: targetAccountId })
      if (disposed)
        return
      onAccountUpdated(result.account)
      if (accountId.value === targetAccountId && await load()) {
        ZNotification.success({
          message: t('accounts.actions.quotaRefreshed'),
        })
      }
    }
    catch (cause) {
      if (!disposed && accountId.value === targetAccountId) {
        if (!report.value)
          error.value = errorMessage(cause)
      }
    }
    finally {
      refreshing.value = false
    }
  }

  watch([open, accountId], ([isOpen]) => {
    request.invalidate({ resetLoading: isOpen })
    if (!isOpen)
      return
    report.value = null
    error.value = ''
    void load()
  }, { immediate: true })

  onScopeDispose(() => {
    disposed = true
  })

  return {
    report,
    loading,
    refreshing,
    error,
    load,
    refresh,
  }
}
