import type { AccountImportSettings, AccountOAuthStartResponse } from '@/api'
import { computed, onScopeDispose, shallowRef } from 'vue'
import { completeAccountOAuth, startAccountOAuth } from '@/api'
import { errorMessage } from '@/utils/operation'

type Status = 'idle' | 'starting' | 'waiting' | 'paused' | 'expired' | 'completing'

export interface AccountAuthorizationView {
  flow: AccountOAuthStartResponse | null
  status: Status
  error: string
}

interface AuthorizationRequest {
  start: Parameters<typeof startAccountOAuth>[0]
  settings?: AccountImportSettings
}

export function useAccountAuthorization(onComplete: () => Promise<void>) {
  const flow = shallowRef<AccountOAuthStartResponse | null>(null)
  const status = shallowRef<Status>('idle')
  const error = shallowRef('')
  const callback = shallowRef('')
  const busy = computed(() => status.value === 'starting' || status.value === 'completing')
  const view = computed<AccountAuthorizationView>(() => ({
    flow: flow.value,
    status: status.value,
    error: error.value,
  }))
  let context: { provider: string, settings?: AccountImportSettings } | undefined
  let controller: AbortController | undefined
  let expiryTimer: ReturnType<typeof setTimeout> | undefined

  function clearTimers() {
    clearTimeout(expiryTimer)
    expiryTimer = undefined
  }

  function cancel() {
    controller?.abort()
    controller = undefined
    clearTimers()
    context = undefined
  }

  function reset() {
    cancel()
    flow.value = null
    status.value = 'idle'
    error.value = ''
    callback.value = ''
  }

  function expired() {
    return !flow.value || Date.parse(flow.value.expiresAt) <= Date.now()
  }

  async function start(request: AuthorizationRequest) {
    reset()
    const current = new AbortController()
    controller = current
    status.value = 'starting'
    try {
      const result = await startAccountOAuth(request.start, { signal: current.signal, silent: true })
      if (current.signal.aborted)
        return
      const expiresAt = Date.parse(result.expiresAt)
      if (!Number.isFinite(expiresAt))
        throw new Error('授权有效期无效，请重新生成链接')
      context = { provider: request.start.provider, settings: request.settings }
      flow.value = result
      status.value = expired() ? 'expired' : 'waiting'
      expiryTimer = setTimeout(() => {
        // 不取消正在确认的提交，过期后仍接收服务端已经提交的结果。
        if (status.value !== 'completing')
          status.value = 'expired'
      }, Math.max(0, Math.min(expiresAt - Date.now(), 2_147_483_647)))
    }
    catch (cause) {
      if (!current.signal.aborted) {
        error.value = errorMessage(cause, '生成授权链接失败')
        status.value = 'idle'
      }
    }
  }

  async function complete() {
    const current = controller
    const currentFlow = flow.value
    if (!current || !currentFlow || !context || busy.value)
      return
    if (!callback.value.trim()) {
      error.value = '请粘贴回调地址或授权码'
      return
    }
    status.value = 'completing'
    error.value = ''
    try {
      await completeAccountOAuth({
        ...context,
        flowId: currentFlow.flowId,
        callbackUrl: callback.value.trim(),
      }, { signal: current.signal, silent: true })
      if (!current.signal.aborted) {
        clearTimers()
        await onComplete()
      }
    }
    catch (cause) {
      if (!current.signal.aborted) {
        status.value = expired() ? 'expired' : 'paused'
        error.value = errorMessage(cause, '完成授权失败，可重试提交')
      }
    }
  }

  onScopeDispose(reset)
  return { view, callback, busy, start, complete, cancel, reset }
}
