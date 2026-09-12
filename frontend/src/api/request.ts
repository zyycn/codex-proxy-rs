import type {
  AxiosError,
  AxiosInstance,
  AxiosRequestConfig,
  AxiosResponse,
} from 'axios'
import type { ApiError } from './error'

import axios from 'axios'
import { toast } from '@/components/base/BaseToast'
import { API_BASE_URL, API_TIMEOUT_MS } from './constants'
import { normalizeApiError, normalizeApiResponseError } from './error'

export { ApiError } from './error'

export interface RequestOptions {
  // 静默只关闭全局提示，不吞掉异常，也不跳过会话失效处理。
  silent?: boolean
  signal?: AbortSignal
  timeout?: number
}

type RequestConfig = AxiosRequestConfig & RequestOptions

const http: AxiosInstance = axios.create({
  baseURL: API_BASE_URL,
  timeout: API_TIMEOUT_MS,
  withCredentials: true,
})

let unauthorizedHandled = false
let unauthorizedHandler: (() => void | Promise<void>) | undefined

export function setUnauthorizedHandler(handler: () => void | Promise<void>) {
  unauthorizedHandler = handler
}

export function resetUnauthorizedHandling() {
  unauthorizedHandled = false
}

function isAuthenticationRequest(url?: string) {
  return Boolean(url?.includes('/api/admin/auth/login') || url?.includes('/api/admin/auth/status'))
}

function handleUnauthorizedOnce() {
  if (unauthorizedHandled || !unauthorizedHandler)
    return
  unauthorizedHandled = true
  void Promise.resolve(unauthorizedHandler()).catch(() => {
    unauthorizedHandled = false
  })
}

http.interceptors.response.use(
  (response: AxiosResponse<unknown>) => {
    const error = normalizeApiResponseError(response)
    if (error)
      return rejectRequest(error, response.config)
    return response
  },
  (error: AxiosError<unknown>) => {
    return rejectRequest(normalizeApiError(error), error.config)
  },
)

function rejectRequest(error: ApiError, config?: AxiosRequestConfig & Pick<RequestOptions, 'silent'>) {
  if (error.kind === 'cancelled' || config?.signal?.aborted)
    return Promise.reject(error)

  const sessionExpired = error.status === 401 && !isAuthenticationRequest(config?.url)
  const alreadyHandled = sessionExpired && unauthorizedHandled
  if (sessionExpired)
    handleUnauthorizedOnce()
  if (!config?.silent && !alreadyHandled)
    toast.error(error.message)
  return Promise.reject(error)
}

interface ApiEnvelope {
  code: number
  message: string
  data: unknown
}

function isApiEnvelope(value: unknown): value is ApiEnvelope {
  return (
    typeof value === 'object'
    && value !== null
    && 'data' in value
    && 'code' in value && typeof value.code === 'number'
    && 'message' in value && typeof value.message === 'string'
  )
}

export default async function request<T = unknown>(config: RequestConfig): Promise<T> {
  const response = await http.request<unknown>({
    ...config,
  })

  if (isApiEnvelope(response.data)) {
    return response.data.data as T
  }

  return response.data as T
}
