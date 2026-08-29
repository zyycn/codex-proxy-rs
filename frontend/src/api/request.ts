import type {
  AxiosError,
  AxiosInstance,
  AxiosRequestConfig,
  AxiosResponse,
} from 'axios'
import axios from 'axios'

import { API_BASE_URL, API_TIMEOUT_MS } from './constants'
import { normalizeApiError } from './error'

export { ApiError } from './error'

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
  (response: AxiosResponse) => {
    return response
  },
  (error: AxiosError<unknown>) => {
    const { response } = error

    const status = response?.status ?? 0

    if (status === 401 && !isAuthenticationRequest(error.config?.url)) {
      handleUnauthorizedOnce()
    }

    return Promise.reject(normalizeApiError(error))
  },
)

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
    && 'code' in value
    && 'message' in value
  )
}

export default async function request<T = unknown>(config: AxiosRequestConfig): Promise<T> {
  const response = await http.request<unknown>({
    ...config,
  })

  if (isApiEnvelope(response.data)) {
    return response.data.data as T
  }

  return response.data as T
}
