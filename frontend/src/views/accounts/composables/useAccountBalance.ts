import type { Ref } from 'vue'
import type { Account, AccountBalance } from '@/api'
import { useDocumentVisibility, useTimeoutPoll } from '@vueuse/core'
import { shallowRef, watch } from 'vue'
import { getAccountBalance, refreshAccountBalance } from '@/api'
import { useRequestState } from '@/composables/useRequestState'
import { isOpenAiApiKeyAccount } from '../utils/upstreamApiKey'

export function useAccountBalance(account: Ref<Account>, onRefreshed: () => void) {
  const balance = shallowRef<AccountBalance | null>()
  const request = useRequestState()

  async function load(refresh = false, background = false) {
    const requestId = request.start(background)
    try {
      const result = await (refresh ? refreshAccountBalance : getAccountBalance)(
        { accountId: account.value.id },
        { signal: request.signal, silent: true },
      )
      if (!request.isCurrent(requestId))
        return
      const observationChanged = balance.value !== undefined && result.balance?.observedAt !== balance.value?.observedAt
      const percentageWindowsMissing = account.value.quota.windows.length === 0
        && result.balance?.entries.some(entry => entry.unit === '%')
      if (result.balance?.observedAt !== balance.value?.observedAt)
        request.error.value = ''
      balance.value = result.balance
      if (refresh || observationChanged || percentageWindowsMissing)
        onRefreshed()
    }
    catch (cause) {
      request.fail(requestId, cause, background)
    }
    finally {
      request.finish(requestId)
    }
  }

  watch([() => account.value.id, () => account.value.updatedAt], () => {
    request.invalidate()
    if (!isOpenAiApiKeyAccount(account.value)) {
      balance.value = null
      return
    }
    balance.value = undefined
    void load()
  }, { immediate: true })

  const visibility = useDocumentVisibility()
  const poll = useTimeoutPoll(async () => {
    if (!request.loading.value)
      await load(false, true)
  }, 30_000, { immediate: false })
  watch([visibility, () => balance.value !== null], ([visible, enabled]) => {
    if (visible === 'visible' && enabled)
      poll.resume()
    else
      poll.pause()
  }, { immediate: true })

  return { balance, loading: request.loading, error: request.error, load }
}
