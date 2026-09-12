import { onScopeDispose, shallowRef } from 'vue'

import { ApiError } from '@/api/request'
import { errorMessage } from '@/utils/async'

export function useRequestState(onError?: (error: unknown) => void) {
  const loading = shallowRef(false)
  const error = shallowRef('')
  let sequence = 0
  let controller: AbortController | undefined

  function start(silent = false) {
    const requestId = ++sequence
    controller?.abort()
    controller = new AbortController()
    if (!silent) {
      loading.value = true
      error.value = ''
    }
    return requestId
  }

  function isCurrent(requestId: number) {
    return requestId === sequence
  }

  function fail(requestId: number, cause: unknown, silent = false) {
    if (!isCurrent(requestId) || silent || (cause instanceof ApiError && cause.kind === 'cancelled'))
      return
    error.value = errorMessage(cause)
    onError?.(cause)
  }

  function finish(requestId: number) {
    if (isCurrent(requestId))
      loading.value = false
  }

  function invalidate() {
    sequence += 1
    controller?.abort()
    controller = undefined
    loading.value = false
  }

  onScopeDispose(invalidate)

  return {
    loading,
    error,
    start,
    isCurrent,
    fail,
    finish,
    invalidate,
    get signal() {
      return controller?.signal
    },
  }
}
