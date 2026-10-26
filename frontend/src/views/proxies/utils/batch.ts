import type { OutboundProxyRecord, ProxyBatchResult } from '@/api'
import { computed } from 'vue'
import { ApiError } from '@/api/request'
import { t } from '@/i18n'

export const MAX_BATCH_CREATE = 100
export const MAX_BATCH_INPUT_BYTES = 512 * 1024
export const proxyProtocolOptions = [
  { label: 'HTTP', value: 'http' },
  { label: 'HTTPS', value: 'https' },
  { label: 'SOCKS5', value: 'socks5' },
  { label: 'SOCKS5H', value: 'socks5h' },
]

export type ProxyBatchStatus = 'pending' | 'running' | 'succeeded' | 'failed' | 'error' | 'conflict' | 'busy' | 'unknown' | 'skipped'

export interface ProxyBatchItem {
  key: string
  name: string
  endpoint: string
  status: ProxyBatchStatus
  message: string
  record?: OutboundProxyRecord
}

export interface ProxyBatchInput {
  line: number
  raw: string
  name: string
  proxyUrl: string
  endpoint: string
  error: string
}

export const proxyBatchStates = computed<Record<ProxyBatchStatus, { label: string, tone: string }>>(() => ({
  pending: {
    label: t('proxies.batchTest.toBeExecuted'),
    tone: 'text-cp-text-secondary',
  },
  running: {
    label: t('common.status.running'),
    tone: 'text-cp-info-text',
  },
  succeeded: {
    label: t('common.status.success'),
    tone: 'text-cp-success-text',
  },
  failed: {
    label: t('common.status.failed'),
    tone: 'text-cp-error-text',
  },
  error: {
    label: t('proxies.batchTest.requestFailed'),
    tone: 'text-cp-error-text',
  },
  conflict: {
    label: t('proxies.batchTest.configurationConflict'),
    tone: 'text-cp-warning-text',
  },
  busy: {
    label: t('proxies.batchTest.testingIsBusy'),
    tone: 'text-cp-warning-text',
  },
  unknown: {
    label: t('proxies.batchTest.resultToBeConfirmed'),
    tone: 'text-cp-warning-text',
  },
  skipped: {
    label: t('proxies.batchTest.notExecuted'),
    tone: 'text-cp-text-secondary',
  },
}))

// 简写只转换录入格式，完整 URL 原样交给后端的 OutboundProxy 校验
function proxyUrlFromLine(value: string, protocol: string): string {
  if (/^[a-z][a-z\d+.-]*:\/\//iu.test(value))
    return value
  const fields = /^(\[[^\]]+\]|[^:\s]+):(\d+)(?::([^:]*):(.*))?$/u.exec(value)
  if (!fields)
    throw new Error(t('proxies.batchTest.invalidProxyFormat'))
  const [, host, port, username, password] = fields
  const url = new URL(`${protocol}://${host}:${port}`)
  if (url.username || url.password || (url.pathname && url.pathname !== '/') || url.search || url.hash)
    throw new Error(t('proxies.batchTest.invalidAddress'))
  url.username = encodeURIComponent(username ?? '')
  url.password = encodeURIComponent(password ?? '')
  return url.toString()
}

function safeEndpoint(proxyUrl: string): string {
  try {
    const url = new URL(proxyUrl)
    url.username = ''
    url.password = ''
    url.pathname = ''
    url.search = ''
    url.hash = ''
    return url.toString()
  }
  catch {
    return ''
  }
}

export function parseProxyBatch(text: string, prefix: string, protocol: string): ProxyBatchInput[] {
  const seen = new Map<string, number>()
  let ordinal = 0
  return text.split(/\r\n|\n|\r/u).flatMap((raw, index) => {
    const value = raw.trim()
    if (!value)
      return []
    const line = index + 1
    const name = `${prefix.trim() || t('proxies.batchCreate.proxy')}-${String(++ordinal).padStart(3, '0')}`
    let proxyUrl = ''
    let error = ''
    try {
      proxyUrl = proxyUrlFromLine(value, protocol)
      const earlier = seen.get(proxyUrl)
      if (earlier !== undefined)
        error = t('proxies.batchTest.duplicateOfLine', { earlier })
      else
        seen.set(proxyUrl, line)
    }
    catch {
      // 解析器的异常可能携带认证信息，界面只显示固定格式说明
      error = t('proxies.batchTest.formatHelp')
    }
    return [
      {
        line,
        raw: value,
        name,
        proxyUrl,
        endpoint: safeEndpoint(proxyUrl),
        error,
      },
    ]
  })
}

export function proxyBatchInputError(text: string, count: number): string {
  if (new TextEncoder().encode(text).length > MAX_BATCH_INPUT_BYTES)
    return t('proxies.batchTest.proxyListCannotExceedKib')
  if (count > MAX_BATCH_CREATE)
    return t('proxies.batchTest.addUpToProxiesAtTime', { value: MAX_BATCH_CREATE })
  return ''
}

export function proxyRequestFailure(error: unknown): { status: ProxyBatchStatus, message: string, stop: boolean } {
  if (!(error instanceof ApiError)) {
    return {
      status: 'unknown',
      message: t('proxies.batchTest.resultUnknown'),
      stop: true,
    }
  }
  if (error.status === 401 || error.status === 403)
    return { status: 'error', message: error.message, stop: true }
  if (error.kind === 'timeout' || error.kind === 'network' || error.kind === 'cancelled' || error.status >= 500) {
    return {
      status: 'unknown',
      message: t('proxies.batchTest.interrupted'),
      stop: true,
    }
  }
  if (error.status === 409)
    return { status: 'conflict', message: error.message, stop: false }
  if (error.status === 429)
    return { status: 'busy', message: error.message, stop: false }
  return { status: 'error', message: error.message, stop: false }
}

export function proxyBatchFailure(result: ProxyBatchResult<unknown> | undefined) {
  if (result?.status === 'skipped') {
    return {
      status: 'skipped' as const,
      message: t('proxies.batchTest.skippedAfterFailure'),
      stop: true,
    }
  }
  return proxyRequestFailure(result?.status === 'failed'
    ? new ApiError(result.message, result.httpStatus, result.code)
    : undefined)
}

export function proxyResultData<T>(result: ProxyBatchResult<T> | undefined): T {
  if (result?.status === 'succeeded')
    return result.data
  throw new Error(proxyBatchFailure(result).message)
}
