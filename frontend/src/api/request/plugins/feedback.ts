import type { RequestContext, RequestNext } from '../types'
import { notification as ZNotification } from '@/plugins/notification'
import { ApiError, isSessionRequired } from './error'

// 共享刷新 Promise 的同一个失败只提示一次，弱引用不保留已结束请求
const reported = new WeakSet<ApiError>()

export async function feedbackPlugin(context: RequestContext, next: RequestNext) {
  try {
    return await next()
  }
  catch (error) {
    if (error instanceof ApiError && error.kind !== 'cancelled' && !isSessionRequired(error)
      && !context.config.silent && context.isCurrent() && !reported.has(error)) {
      reported.add(error)
      ZNotification.error({ message: error.message })
    }
    throw error
  }
}
