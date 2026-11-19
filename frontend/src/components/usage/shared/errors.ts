import type { UsageErrorFacts } from '@/api/modules/usage'
import { t } from '@/i18n'

export type UsageErrorRecord = UsageErrorFacts & {
  provider: string | null
  logicalOutcome: string
}

export function usageDisplayOutcome(outcome: string, errorCode: string | null, errorKind: string | null): string {
  // 已开始交付的请求仍可能明确失败，展示不改变原始终态
  // 进程中断和缺少明确错误的断流保留未完成，不从 HTTP 状态或文案推断
  if (outcome === 'incomplete' && errorKind !== 'process_interrupted' && (errorCode || errorKind === 'timeout'))
    return 'failed'
  return outcome
}

export function usageErrorTitle(record: Pick<UsageErrorRecord, 'logicalOutcome' | 'errorCode' | 'errorKind'>): string {
  return usageDisplayOutcome(record.logicalOutcome, record.errorCode, record.errorKind) === 'incomplete'
    ? t('usage.error.responseIsNotComplete')
    : t('usage.error.requestFailed')
}

export function usageErrorOrigin(record: Pick<UsageErrorFacts, 'errorOrigin'>) {
  const origins = {
    proxy: {
      label: t('usage.error.origin.proxy'),
      className: 'text-z-blue-text',
      hint: null,
    },
    upstream: {
      label: t('usage.error.origin.upstream'),
      className: 'text-z-error-text',
      hint: null,
    },
    connection: {
      label: t('usage.error.origin.connection'),
      className: 'text-z-warning-text',
      hint: t('usage.error.origin.connectionHint'),
    },
    unknown: {
      label: t('usage.error.origin.unknown'),
      className: 'text-z-text-secondary',
      hint: t('usage.error.origin.unknownHint'),
    },
  }
  return { kind: record.errorOrigin, ...origins[record.errorOrigin] }
}

export function usageSendStateText(state: string) {
  switch (state) {
    case 'not_sent': return t('usage.error.sendState.notSent')
    case 'sent': return t('usage.error.sendState.sent')
    case 'ambiguous': return t('usage.error.sendState.ambiguous')
    case 'unknown': return t('usage.error.responseNotRecorded')
    default: return state
  }
}

export function usageErrorReason(code: string | null): string | undefined {
  switch (code) {
    case 'no_available_provider': return t('usage.error.noAvailableProvider')
    case 'rate_limited':
    case 'rate_limit_exceeded': return t('common.errors.rateLimited')
    case 'invalid_request': return t('common.errors.invalidParameters')
    case 'upstream_connection_closed': return t('usage.error.upstreamConnectionClosed')
    case 'upstream_timeout': return t('common.errors.upstreamTimeout')
    case 'timeout':
    case 'request_timeout': return t('common.errors.timeout')
    case 'concurrency_queue_timeout': return t('usage.error.queueTimeout')
    case 'concurrency_queue_full': return t('usage.error.queueFull')
    case 'message_too_big': return t('common.errors.payloadTooLarge')
    case 'cancelled':
    case 'request_cancelled': return t('common.errors.canceled')
    case 'internal_error': return t('common.errors.internal')
  }
}
