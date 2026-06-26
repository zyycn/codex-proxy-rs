import axios, {
  AxiosError,
  type AxiosInstance,
  type AxiosRequestConfig,
  type AxiosResponse,
  type InternalAxiosRequestConfig,
} from 'axios'

class ApiError extends Error {
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

interface RequestConfig extends AxiosRequestConfig {
  skipErrorHandler?: boolean
  skipAuth?: boolean
}

const http: AxiosInstance = axios.create({
  baseURL: import.meta.env.DEV ? '/dev' : '',
  timeout: 30000,
  withCredentials: true,
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
  (error: AxiosError<any>) => {
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

function isApiEnvelope(value: any): value is { data: any } {
  return (
    typeof value === 'object' &&
    value !== null &&
    'data' in value &&
    'code' in value &&
    'message' in value
  )
}

export default async function request<T = any>(config: RequestConfig): Promise<T> {
  const response = await http.request<any, AxiosResponse<any>>({
    ...config,
  })

  if (isApiEnvelope(response.data)) {
    return response.data.data
  }

  return response.data
}
