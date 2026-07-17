import { shallowRef } from 'vue'

import { toast } from '@/components/base/BaseToast'
import { withMinimumDuration } from '@/utils/async'

type MaybePromise<T> = T | Promise<T>
type AsyncActionErrorText = string | false | ((error: unknown) => string | false)

interface AsyncActionRunOptions {
  errorText?: AsyncActionErrorText
  minimumMs?: number
  onError?: (error: unknown) => void
  rethrow?: boolean
}

function errorMessage(error: unknown) {
  if (error instanceof Error) {
    return error.message
  }

  if (error && typeof error === 'object' && 'message' in error) {
    const message = (error as { message?: unknown }).message
    return typeof message === 'string' ? message : ''
  }

  return ''
}

function resolveErrorText(error: unknown, errorText: AsyncActionErrorText | undefined) {
  const fallback = typeof errorText === 'function' ? errorText(error) : errorText
  if (fallback === false) {
    return ''
  }

  return errorMessage(error) || fallback || ''
}

export function useAsyncAction() {
  const loading = shallowRef(false)

  async function run<T>(task: () => MaybePromise<T>, options: AsyncActionRunOptions = {}) {
    if (loading.value) {
      return undefined
    }

    loading.value = true
    try {
      const execute = async () => task()
      return options.minimumMs === undefined
        ? await execute()
        : await withMinimumDuration(execute, options.minimumMs)
    }
    catch (error) {
      options.onError?.(error)

      const message = resolveErrorText(error, options.errorText)
      if (message) {
        toast.error(message)
      }

      if (options.rethrow) {
        throw error
      }
      return undefined
    }
    finally {
      loading.value = false
    }
  }

  return {
    loading,
    run,
  }
}
