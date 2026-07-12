import axios, {
  AxiosError,
  type AxiosInstance,
  type AxiosRequestConfig,
  type AxiosResponse,
} from 'axios'

import { API_BASE_URL, API_TIMEOUT_MS } from './constants'

export type ApiPayload = Record<string, unknown>

interface ApiEnvelope<T = unknown> {
  code: number
  message: string
  data: T
}

interface ApiErrorBody {
  code?: number
  message?: string
  data?: unknown
}

export class ApiError extends Error {
  constructor(
    message: string,
    public readonly status: number,
    public readonly code?: number,
    public readonly requestId?: string,
  ) {
    super(message)
    this.name = 'ApiError'
  }
}

const http: AxiosInstance = axios.create({
  baseURL: API_BASE_URL,
  timeout: API_TIMEOUT_MS,
  withCredentials: true,
})

let unauthorizedHandled = false

export function resetUnauthorizedHandling() {
  unauthorizedHandled = false
}

function isAuthenticationRequest(url?: string) {
  return Boolean(url?.includes('/api/admin/login') || url?.includes('/api/admin/auth/status'))
}

function handleUnauthorizedOnce() {
  if (unauthorizedHandled) return
  unauthorizedHandled = true
  void Promise.all([import('@/stores/modules/auth'), import('@/router')])
    .then(async ([{ useAuthStore }, { router }]) => {
      useAuthStore().invalidateSession()
      if (router.currentRoute.value.path !== '/login') {
        await router.replace({ name: 'login' })
      }
    })
    .catch(() => {
      unauthorizedHandled = false
    })
}

http.interceptors.response.use(
  (response: AxiosResponse) => {
    return response
  },
  (error: AxiosError<ApiErrorBody>) => {
    const { response } = error

    const status = response?.status || 0
    const message = response?.data?.message || error.message || '请求失败'
    const code = response?.data?.code
    const requestId = response?.headers?.['x-request-id']

    if (status === 401 && !isAuthenticationRequest(error.config?.url)) {
      handleUnauthorizedOnce()
    }

    return Promise.reject(new ApiError(message, status, code, requestId))
  },
)

function isApiEnvelope(value: unknown): value is ApiEnvelope {
  return (
    typeof value === 'object' &&
    value !== null &&
    'data' in value &&
    'code' in value &&
    'message' in value
  )
}

export default async function request<T = any>(config: AxiosRequestConfig): Promise<T> {
  const response = await http.request<unknown, AxiosResponse<unknown>>({
    ...config,
  })

  if (isApiEnvelope(response.data)) {
    return response.data.data as T
  }

  return response.data as T
}
