import type {
  AxiosInstance,
  AxiosRequestConfig,
} from 'axios'

import axios from 'axios'
import { toast } from '@/components/base/BaseToast'
import { API_BASE_URL, API_TIMEOUT_MS } from './constants'
import { ApiError, normalizeApiError, normalizeApiResponseError } from './error'

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

// 会话失效是后端业务事实；登录凭据错误和 403 不清除已有会话。
const SESSION_REQUIRED = 40101
let unauthorizedHandled = false
let sessionGeneration = 0
let unauthorizedHandler: (() => void | Promise<void>) | undefined

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

function rejectRequest(error: ApiError, config: RequestConfig, generation: number) {
  if (error.kind === 'cancelled' || config.signal?.aborted || generation !== sessionGeneration)
    return Promise.reject(error)

  const sessionExpired = error.status === 401 && error.code === SESSION_REQUIRED
  const alreadyHandled = sessionExpired && unauthorizedHandled
  if (sessionExpired)
    handleUnauthorizedOnce()
  if (!config.silent && !alreadyHandled)
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
  const generation = sessionGeneration
  try {
    const response = await http.request<unknown>(config)
    const error = normalizeApiResponseError(response)
    if (error)
      throw error
    return (isApiEnvelope(response.data) ? response.data.data : response.data) as T
  }
  catch (error) {
    if (error instanceof ApiError)
      return rejectRequest(error, config, generation)
    if (axios.isAxiosError(error))
      return rejectRequest(normalizeApiError(error), config, generation)
    throw error
  }
}
