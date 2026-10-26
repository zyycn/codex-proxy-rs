import type { RequestConfig } from '../index'
import type { RequestContext, RequestNext } from './index'
import { t } from '@/i18n'
import { notification as ZNotification } from '@/utils/notification'
import { ApiError, isSessionRequired } from './error'

// 会话失效是后端业务事实，登录凭据错误和 403 不清除已有会话
const SESSION_REQUIRED = 40101
let unauthorizedHandled = false
let sessionGeneration = 0
let unauthorizedHandler: (() => void | Promise<void>) | undefined
let sessionRecoveryHandler: (() => Promise<boolean>) | undefined
let sessionRecovery: Promise<boolean> | undefined
let successfulRecoveries = 0

export function setSessionRecoveryHandler(handler: () => Promise<boolean>) {
  sessionRecoveryHandler = handler
}

export function setUnauthorizedHandler(handler: () => void | Promise<void>) {
  unauthorizedHandler = handler
}

export function resetUnauthorizedHandling() {
  sessionGeneration += 1
  unauthorizedHandled = false
}

function handleUnauthorizedOnce() {
  if (unauthorizedHandled || !unauthorizedHandler)
    return
  unauthorizedHandled = true
  void Promise.resolve(unauthorizedHandler()).catch(() => {
    unauthorizedHandled = false
  })
}

function recoverSession(): Promise<boolean> {
  if (sessionRecovery)
    return sessionRecovery
  const recovery = sessionRecoveryHandler!().then((authenticated) => {
    if (authenticated)
      successfulRecoveries += 1
    return authenticated
  }).finally(() => {
    if (sessionRecovery === recovery)
      sessionRecovery = undefined
  })
  sessionRecovery = recovery
  return recovery
}

function expireSession(error: ApiError, config: RequestConfig, generation: number): never {
  if (!config.signal?.aborted && generation === sessionGeneration) {
    const alreadyHandled = unauthorizedHandled
    handleUnauthorizedOnce()
    if (!config.silent && !alreadyHandled)
      ZNotification.error({ message: error.message })
  }
  throw error
}

export async function sessionPlugin(context: RequestContext, send: RequestNext) {
  const { config } = context
  const generation = sessionGeneration
  const isCurrent = context.isCurrent
  context.isCurrent = () => isCurrent() && generation === sessionGeneration
  if (!config.skipSessionRecovery && sessionRecovery && !await sessionRecovery)
    return expireSession(new ApiError(t('common.errors.sessionExpired'), 401, SESSION_REQUIRED), config, generation)
  if (config.signal?.aborted || generation !== sessionGeneration)
    throw new ApiError(t('common.errors.canceled'), 0, undefined, undefined, 'cancelled')
  const sendCurrent: RequestNext = async () => {
    const response = await send()
    if (context.isCurrent())
      return response
    // 身份已切换时丢弃迟到结果，流式传输由请求层终止
    throw new ApiError(t('common.errors.canceled'), 0, undefined, undefined, 'cancelled')
  }
  const recovered = successfulRecoveries
  try {
    return await sendCurrent()
  }
  catch (error) {
    if (!isSessionRequired(error) || config.signal?.aborted || generation !== sessionGeneration)
      throw error
    if (!config.skipSessionRecovery && sessionRecoveryHandler
      && await (sessionRecovery ?? (successfulRecoveries !== recovered
        ? true
        : recoverSession()))) {
      // 迟到的 40101 也须等待当前续期；Cookie 更新完成后原请求最多重放一次
      if (config.signal?.aborted || generation !== sessionGeneration)
        throw error
      try {
        return await sendCurrent()
      }
      catch (retryError) {
        if (!isSessionRequired(retryError))
          throw retryError
        return expireSession(retryError, config, generation)
      }
    }
    return expireSession(error, config, generation)
  }
}
