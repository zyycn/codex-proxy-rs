import type {
  AxiosError,
  AxiosInstance,
  AxiosRequestConfig,
  AxiosResponse,
} from 'axios'
import type { ZodType } from 'zod'
import axios from 'axios'

import { API_BASE_URL, API_TIMEOUT_MS } from './constants'

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

type UnauthorizedHandler = () => void | Promise<void>

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
let unauthorizedHandler: UnauthorizedHandler | undefined

export function setUnauthorizedHandler(handler: UnauthorizedHandler) {
  unauthorizedHandler = handler
}

export function resetUnauthorizedHandling() {
  unauthorizedHandled = false
}

function isAuthenticationRequest(url?: string) {
  return Boolean(url?.includes('/api/admin/login') || url?.includes('/api/admin/auth/status'))
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
    typeof value === 'object'
    && value !== null
    && 'data' in value
    && 'code' in value
    && 'message' in value
  )
}

export default async function request(config: AxiosRequestConfig) {
  const response = await http.request<unknown, AxiosResponse<unknown>>({
    ...config,
  })

  if (isApiEnvelope(response.data)) {
    return response.data.data
  }

  return response.data
}

export async function requestParsed<const Schema extends ZodType>(
  config: AxiosRequestConfig,
  schema: Schema,
) {
  return schema.parse(await request(config))
}
