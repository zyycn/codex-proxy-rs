import axios, {
  AxiosError,
  type AxiosInstance,
  type AxiosRequestConfig,
  type AxiosResponse,
  type InternalAxiosRequestConfig,
} from 'axios'

import type { ApiEnvelope, ApiPageEnvelope, PaginatedResult } from './types'

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

export interface RequestConfig extends AxiosRequestConfig {
  skipErrorHandler?: boolean
  skipAuth?: boolean
}

const http: AxiosInstance = axios.create({
  baseURL: import.meta.env.VITE_API_BASE_URL || '/',
  timeout: 30000,
  withCredentials: true,
  headers: {
    'Content-Type': 'application/json',
    Accept: 'application/json',
  },
})

http.interceptors.request.use(
  (config: InternalAxiosRequestConfig) => {
    if (config.method === 'get') {
      config.params = {
        ...config.params,
        _t: Date.now(),
      }
    }

    return config
  },
  (error: AxiosError) => {
    console.error('Request error:', error)
    return Promise.reject(error)
  },
)

http.interceptors.response.use(
  (response: AxiosResponse) => {
    return response
  },
  (error: AxiosError<ApiEnvelope<unknown>>) => {
    const { response, config } = error

    if ((config as RequestConfig)?.skipErrorHandler) {
      return Promise.reject(error)
    }

    const status = response?.status || 0
    const message = response?.data?.message || error.message || '请求失败'
    const code = response?.data?.code
    const requestId = response?.headers?.['x-request-id']

    switch (status) {
      case 401:
        console.warn('Unauthorized, redirecting to login...')
        break
      case 403:
        console.error('Forbidden:', message)
        break
      case 404:
        console.error('Not Found:', message)
        break
      case 500:
        console.error('Server Error:', message)
        break
      default:
        break
    }

    return Promise.reject(new ApiError(message, status, code, requestId))
  },
)

function isApiEnvelope<T>(value: unknown): value is ApiEnvelope<T> {
  return (
    typeof value === 'object' &&
    value !== null &&
    'data' in value &&
    'code' in value &&
    'message' in value
  )
}

function isApiPageEnvelope<T>(value: unknown): value is ApiPageEnvelope<T> {
  return (
    isApiEnvelope<unknown>(value) &&
    typeof value.data === 'object' &&
    value.data !== null &&
    'items' in value.data &&
    'page' in value.data
  )
}

export async function requestJson<T = any, D = any>(
  url: string,
  config?: RequestConfig,
): Promise<T> {
  const response = await http.request<ApiEnvelope<T> | T, AxiosResponse<ApiEnvelope<T> | T>, D>({
    url,
    ...config,
  })

  if (isApiEnvelope<T>(response.data)) {
    return response.data.data
  }

  return response.data
}

export async function requestPageJson<T = any, D = any>(
  url: string,
  config?: RequestConfig,
): Promise<PaginatedResult<T>> {
  const response = await http.request<ApiPageEnvelope<T>, AxiosResponse<ApiPageEnvelope<T>>, D>({
    url,
    ...config,
  })

  if (!isApiPageEnvelope<T>(response.data)) {
    throw new ApiError('分页响应格式无效', response.status)
  }

  return {
    items: response.data.data.items,
    page: response.data.data.page,
  }
}

export function get<T = any>(url: string, params?: any, config?: RequestConfig): Promise<T> {
  return requestJson<T>(url, {
    method: 'GET',
    params,
    ...config,
  })
}

export function post<T = any, D = any>(url: string, data?: D, config?: RequestConfig): Promise<T> {
  return requestJson<T, D>(url, {
    method: 'POST',
    data,
    ...config,
  })
}

export { http }

export default {
  http,
  request: requestJson,
  requestPage: requestPageJson,
  get,
  post,
}
