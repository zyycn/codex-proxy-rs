import type { AxiosError, AxiosResponseHeaders, RawAxiosResponseHeaders } from 'axios'

export type ApiErrorKind = 'api' | 'timeout' | 'network' | 'http' | 'cancelled'

interface AdminErrorEnvelope {
  code: number
  message: string
}

export class ApiError extends Error {
  constructor(
    message: string,
    public readonly status: number,
    public readonly code?: number,
    public readonly requestId?: string,
    public readonly kind: ApiErrorKind = 'api',
  ) {
    super(message)
    this.name = 'ApiError'
  }
}

export function normalizeApiError(error: AxiosError<unknown>) {
  const status = error.response?.status ?? 0
  const requestId = responseHeader(error.response?.headers, 'x-request-id')
  const envelope = adminErrorEnvelope(error.response?.data)

  if (envelope) {
    return new ApiError(envelope.message, status, envelope.code, requestId, 'api')
  }
  if (error.code === 'ECONNABORTED' || error.code === 'ETIMEDOUT' || error.code === 'ERR_TIMEOUT') {
    return new ApiError('请求超时，请稍后重试', status, undefined, requestId, 'timeout')
  }
  if (error.code === 'ERR_CANCELED') {
    return new ApiError('请求已取消', status, undefined, requestId, 'cancelled')
  }
  if (!error.response) {
    return new ApiError('网络连接失败，请检查网络后重试', 0, undefined, requestId, 'network')
  }
  return new ApiError(httpFallback(status), status, undefined, requestId, 'http')
}

function adminErrorEnvelope(value: unknown): AdminErrorEnvelope | null {
  if (!value || typeof value !== 'object')
    return null
  const record = value as Record<string, unknown>
  if (typeof record.code !== 'number' || typeof record.message !== 'string')
    return null
  const message = record.message.trim()
  return message ? { code: record.code, message } : null
}

function responseHeader(
  headers: RawAxiosResponseHeaders | AxiosResponseHeaders | undefined,
  name: string,
) {
  if (!headers)
    return undefined
  const value = typeof headers.get === 'function' ? headers.get(name) : headers[name]
  return typeof value === 'string' && value ? value : undefined
}

function httpFallback(status: number) {
  switch (status) {
    case 400:
      return '请求参数不合法'
    case 401:
      return '需要管理员登录'
    case 403:
      return '没有权限执行此操作'
    case 404:
      return '请求的接口不存在'
    case 408:
      return '请求超时，请稍后重试'
    case 409:
      return '资源状态冲突，请刷新后重试'
    case 413:
      return '请求内容过大'
    case 415:
      return '请求格式不受支持'
    case 422:
      return '请求内容不合法'
    case 429:
      return '请求过于频繁，请稍后重试'
    case 500:
      return '服务内部错误'
    case 502:
      return '上游服务请求失败'
    case 503:
      return '服务暂不可用，请稍后重试'
    case 504:
      return '上游服务响应超时'
    default:
      return status > 0 ? `请求失败（HTTP ${status}）` : '请求失败'
  }
}
