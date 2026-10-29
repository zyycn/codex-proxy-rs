import type { Ref } from 'vue'
import type { Account } from '@/api'
import { useEventListener } from '@vueuse/core'
import { onScopeDispose, watch } from 'vue'
import { getAccountDetail, refreshAccountUpstreamInfo } from '@/api'

export function useAccountUpstreamRefresh(accounts: Ref<Account[]>) {
  const requests = new Map<string, { controller: AbortController, revision: string }>()

  function cancelAll() {
    for (const { controller } of requests.values())
      controller.abort()
  }

  onScopeDispose(cancelAll)
  useEventListener(document, 'visibilitychange', () => {
    if (document.hidden)
      cancelAll()
  })
  watch(accounts, (current) => {
    for (const [id, request] of requests) {
      const upstream = current.find(account => account.id === id)?.upstream
      if (!upstream?.discovered || upstream.connectionRevision !== request.revision)
        request.controller.abort()
    }
  }, { flush: 'sync' })

  async function refresh(account: Account): Promise<{ account: Account } | { pending: true } | undefined> {
    if (!account.upstream?.discovered || requests.has(account.id) || document.hidden)
      return
    const controller = new AbortController()
    const { signal } = controller
    requests.set(account.id, { controller, revision: account.upstream.connectionRevision })
    let timedOut = false
    const deadline = window.setTimeout(() => {
      timedOut = true
      controller.abort()
    }, 60_000)

    try {
      const accepted = await refreshAccountUpstreamInfo({
        accountIds: [account.id],
      }, { signal, silent: true })
      const ticket = accepted.items.find(item => item.accountId === account.id)
      if (!ticket || ticket.connectionRevision !== account.upstream.connectionRevision)
        return

      // 202 只表示入队，按连接版本与请求代次确认本次查询结束。
      for (let attempt = 0; !signal.aborted; attempt += 1) {
        const { account: updated } = await getAccountDetail({ accountId: account.id }, { signal, silent: true })
        if (signal.aborted)
          return
        const upstream = updated.upstream
        if (!upstream?.discovered || upstream.connectionRevision !== ticket.connectionRevision)
          return
        accounts.value = accounts.value.map(item => item.id === updated.id
          ? updated
          : item)
        if (upstream.sync.completedGeneration >= ticket.requestGeneration)
          return { account: updated }
        await waitForNextRead(Math.min(1000 * 2 ** attempt, 5000), signal)
      }
    }
    catch (error) {
      if (!signal.aborted)
        throw error
    }
    finally {
      window.clearTimeout(deadline)
      requests.delete(account.id)
    }
    return timedOut
      ? { pending: true }
      : undefined
  }

  return { refresh }
}

function waitForNextRead(delay: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    let timer: number | undefined
    const finish = () => {
      window.clearTimeout(timer)
      signal.removeEventListener('abort', finish)
      resolve()
    }
    timer = window.setTimeout(finish, delay)
    signal.addEventListener('abort', finish, { once: true })
    if (signal.aborted)
      finish()
  })
}
