import { onScopeDispose, shallowRef } from 'vue'

import { errorMessage } from '@/utils/async'

export function useRequestState(onError?: (error: unknown) => void) {
  const loading = shallowRef(false)
  const error = shallowRef('')
  let sequence = 0

  function start(silent = false) {
    const requestId = ++sequence
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
    if (!isCurrent(requestId) || silent)
      return
    error.value = errorMessage(cause, '加载失败')
    onError?.(cause)
  }

  function finish(requestId: number) {
    if (isCurrent(requestId))
      loading.value = false
  }

  function invalidate() {
    sequence += 1
    loading.value = false
  }

  onScopeDispose(invalidate)

  return { loading, error, start, isCurrent, fail, finish, invalidate }
}
