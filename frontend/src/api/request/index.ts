import type { AxiosRequestConfig } from 'axios'
import type { RequestContext, RequestNext, RequestPlugin } from './plugins'
import type { RawResponse, RawResponseOptions } from './plugins/raw'
import axios from 'axios'
import { API_BASE_URL, API_TIMEOUT_MS } from '../constants'
import { errorPlugin } from './plugins/error'
import { feedbackPlugin } from './plugins/feedback'
import { rawPlugin } from './plugins/raw'
import { retryPlugin } from './plugins/retry'
import { sessionPlugin } from './plugins/session'

export { ApiError } from './plugins/error'
export type { RawResponse, RawResponseMeta, RawResponseOptions } from './plugins/raw'
export { resetUnauthorizedHandling, setSessionRecoveryHandler, setUnauthorizedHandler } from './plugins/session'

export interface RequestOptions {
  // 静默只关闭提示，不吞掉异常或绕过会话恢复
  silent?: boolean
  signal?: AbortSignal
  timeout?: number
  // 读取默认重试，写操作须由调用方确认幂等后开启
  retry?: boolean
  // 认证接口自身跳过会话恢复，避免循环等待
  skipSessionRecovery?: boolean
}

export type RequestConfig = AxiosRequestConfig & RequestOptions

const http = axios.create({
  baseURL: API_BASE_URL,
  timeout: API_TIMEOUT_MS,
  withCredentials: true,
})

// 从外到内：提示、会话恢复、重试、错误归一化、原始响应处理、实际传输
const plugins: RequestPlugin[] = [
  feedbackPlugin,
  sessionPlugin,
  retryPlugin,
  errorPlugin,
  rawPlugin,
]

function execute(config: RequestConfig, raw?: RawResponseOptions, stream = false) {
  const context: RequestContext = {
    config,
    raw,
    stream,
    isCurrent: () => !config.signal?.aborted,
  }
  const transport: RequestNext = () => http.request<unknown>(context.config)
  return plugins.reduceRight<RequestNext>((next, plugin) => () => plugin(context, next), transport)()
}

export default async function request<T = unknown>(config: RequestConfig): Promise<T> {
  const { data } = await execute(config)
  const envelope = data !== null && typeof data === 'object' && 'data' in data
    && 'code' in data && typeof data.code === 'number'
    && 'message' in data && typeof data.message === 'string'
  return (envelope ? data.data : data) as T
}

export async function requestRaw(config: RequestConfig, options: RawResponseOptions): Promise<RawResponse> {
  const { data } = await execute({ retry: false, ...config }, options)
  return data as RawResponse
}

export async function requestStream(config: RequestConfig, options: RawResponseOptions): Promise<Response> {
  // 成功正文保持流式交付；宿主错误仍由同一管线识别和恢复，禁止网络错误重放写操作
  const controller = new AbortController()
  const signal = config.signal
    ? AbortSignal.any([config.signal, controller.signal])
    : controller.signal
  try {
    const { data } = await execute({
      ...config,
      adapter: 'fetch',
      retry: false,
      signal,
    }, options, true)
    return data as Response
  }
  catch (error) {
    controller.abort()
    throw error
  }
}
