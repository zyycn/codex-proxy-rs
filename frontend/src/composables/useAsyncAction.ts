import { toast } from '@codex-proxy/ui'

import { shallowRef } from 'vue'
import { ApiError } from '@/api/request'
import { errorMessage, withMinimumDuration } from '@/utils/operation'

type MaybePromise<T> = T | Promise<T>
interface AsyncActionRunOptions {
  // 仅用于本地校验、文件与浏览器操作；接口错误由请求层负责提示。
  errorText?: string | false
  minimumMs?: number
  onError?: (error: unknown) => void
  rethrow?: boolean
}

function resolveErrorText(error: unknown, errorText: string | false | undefined) {
  if (error instanceof ApiError || errorText === false) {
    return ''
  }

  return errorMessage(error, errorText || '操作失败')
}

export function useAsyncAction(defaults: AsyncActionRunOptions = {}) {
  const loading = shallowRef(false)

  async function run<T>(task: () => MaybePromise<T>, overrides: AsyncActionRunOptions = {}) {
    if (loading.value) {
      return undefined
    }

    loading.value = true
    const options = { ...defaults, ...overrides }
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
