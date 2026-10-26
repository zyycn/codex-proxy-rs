import type { AxiosError, AxiosResponse, AxiosResponseHeaders, RawAxiosResponseHeaders } from 'axios'
import type { RequestContext, RequestNext } from './index'
import { isAxiosError } from 'axios'
import { t } from '@/i18n'

export type ApiErrorKind = 'api' | 'timeout' | 'network' | 'http' | 'cancelled'

interface AdminErrorEnvelope {
  code: number
  message: string
}

export async function errorPlugin(_context: RequestContext, next: RequestNext) {
  try {
    const response = await next()
    const error = normalizeApiResponseError(response)
    if (error)
      throw error
    return response
  }
  catch (error) {
    throw isAxiosError(error)
      ? normalizeApiError(error)
      : error
  }
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

export function isTransientApiError(error: unknown): error is ApiError {
  return error instanceof ApiError
    && (error.kind === 'network' || error.kind === 'timeout' || [502, 503, 504].includes(error.status))
}

export function normalizeApiError(error: AxiosError<unknown>) {
  const status = error.response?.status ?? 0
  const requestId = responseHeader(error.response?.headers, 'x-request-id')

  if (error.code === 'ERR_CANCELED') {
    return new ApiError(t('common.errors.canceled'), status, undefined, requestId, 'cancelled')
  }
  const apiError = error.response && normalizeApiResponseError(error.response)
  if (apiError)
    return apiError
  if (error.code === 'ECONNABORTED' || error.code === 'ETIMEDOUT' || error.code === 'ERR_TIMEOUT') {
    return new ApiError(t('common.errors.timeout'), status, undefined, requestId, 'timeout')
  }
  if (!error.response) {
    return new ApiError(t('common.errors.network'), 0, undefined, requestId, 'network')
  }
  return new ApiError(httpFallback(status), status, undefined, requestId, 'http')
}

export function normalizeApiResponseError(response: AxiosResponse<unknown>) {
  const envelope = adminErrorEnvelope(response.data)
  if (!envelope || envelope.code === 200)
    return undefined
  const message = adminErrorMessage(envelope) || httpFallback(response.status >= 400
    ? response.status
    : 0)
  return new ApiError(
    message,
    response.status,
    envelope.code,
    responseHeader(response.headers, 'x-request-id'),
    'api',
  )
}

function adminErrorEnvelope(value: unknown): AdminErrorEnvelope | null {
  if (!value || typeof value !== 'object')
    return null
  const record = value as Record<string, unknown>
  if (typeof record.code !== 'number' || !Number.isSafeInteger(record.code))
    return null
  const message = typeof record.message === 'string'
    ? record.message.trim()
    : ''
  return { code: record.code, message }
}

// 固定错误码使用本地文案，业务分类码保留后端已公开的具体原因
function adminErrorMessage(error: AdminErrorEnvelope): string {
  switch (error.code) {
    case 40000: return t('common.errors.malformedJson')
    case 40002: return t('common.errors.invalidTimeRange')
    case 40101: return t('common.errors.signInRequired')
    case 40102: return t('common.errors.invalidCredentials')
    case 40103: return t('common.errors.invalidAdminApiKey')
    case 40301: return t('common.errors.forbidden')
    case 42901: return t('common.errors.tooManyLoginAttempts')
    case 42902: return error.message || t('common.errors.rateLimited')
    case 50001: return t('common.errors.internal')
    case 50201: return error.message || t('common.errors.upstreamFailure')
    case 50202: return error.message || t('common.errors.upstreamResultUnknown')
    case 50301: return error.message || t('common.errors.unavailable')
    default: return error.message
  }
}

function responseHeader(
  headers: RawAxiosResponseHeaders | AxiosResponseHeaders | undefined,
  name: string,
) {
  if (!headers)
    return undefined
  const value = typeof headers.get === 'function'
    ? headers.get(name)
    : headers[name]
  return typeof value === 'string' && value
    ? value
    : undefined
}

function httpFallback(status: number) {
  switch (status) {
    case 400:
      return t('common.errors.invalidParameters')
    case 401:
      return t('common.errors.signInRequired')
    case 403:
      return t('common.errors.forbidden')
    case 404:
      return t('common.errors.notFound')
    case 408:
      return t('common.errors.timeout')
    case 409:
      return t('common.errors.conflict')
    case 413:
      return t('common.errors.payloadTooLarge')
    case 415:
      return t('common.errors.unsupportedFormat')
    case 422:
      return t('common.errors.invalidContent')
    case 429:
      return t('common.errors.rateLimited')
    case 500:
      return t('common.errors.internal')
    case 502:
      return t('common.errors.upstreamFailure')
    case 503:
      return t('common.errors.unavailable')
    case 504:
      return t('common.errors.upstreamTimeout')
    default:
      return status > 0
        ? t('common.errors.http', { status })
        : t('common.errors.request')
  }
}

export function isSessionRequired(error: unknown): error is ApiError & { status: 401, code: 40101 } {
  return error instanceof ApiError && error.status === 401 && error.code === 40101
}
