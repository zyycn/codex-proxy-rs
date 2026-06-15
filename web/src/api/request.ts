import axios, { type AxiosHeaders, type AxiosRequestConfig, type AxiosResponse, type AxiosResponseHeaders } from 'axios'

import type { ApiEnvelope } from './types'

export class ApiError extends Error {
  constructor(
    message: string,
    public readonly status: number,
    public readonly requestId?: string,
  ) {
    super(message)
    this.name = 'ApiError'
  }
}

export const http = axios.create({
  baseURL: '/',
  withCredentials: true,
  headers: {
    Accept: 'application/json',
    'Content-Type': 'application/json',
  },
})

type ResponseHeaders = AxiosResponseHeaders | Partial<AxiosHeaders>

function headerValue(headers: ResponseHeaders | undefined, name: string) {
  if (!headers) return undefined
  const value = headers[name] ?? headers[name.toLowerCase()]
  return typeof value === 'string' ? value : undefined
}

function isApiEnvelope<T>(value: unknown): value is ApiEnvelope<T> {
  return typeof value === 'object'
    && value !== null
    && 'data' in value
    && 'code' in value
    && 'message' in value
}

export async function requestJson<T, D = unknown>(
  url: string,
  config: Omit<AxiosRequestConfig<D>, 'url'> = {},
): Promise<T> {
  try {
    const response = await http.request<ApiEnvelope<T> | T, AxiosResponse<ApiEnvelope<T> | T>, D>({
      url,
      ...config,
      headers: {
        Accept: 'application/json',
        'Content-Type': 'application/json',
        ...config.headers,
      },
    })

    return isApiEnvelope<T>(response.data) ? response.data.data : response.data
  } catch (error) {
    if (axios.isAxiosError<ApiEnvelope<unknown>>(error)) {
      const requestId = headerValue(error.response?.headers, 'x-request-id')
        ?? error.response?.data?.requestId
      throw new ApiError(
        error.response?.data?.message ?? error.message,
        error.response?.status ?? 0,
        requestId,
      )
    }

    throw error
  }
}
