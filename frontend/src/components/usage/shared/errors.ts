import type { UsageErrorFacts, UsageRecord } from '@/api/modules/usage'
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

type ErrorOrigin = 'proxy' | 'upstream' | 'connection' | 'unknown'

// 只按已记录的来源事实展示位置，客户端状态码和文案不用于反推责任方
export function usageErrorOrigin(record: UsageErrorRecord & Partial<Pick<UsageRecord, 'upstreamError' | 'upstreamStatusCode'>>) {
  let kind: ErrorOrigin = 'unknown'
  if (record.errorCode || record.websocketCloseCode != null
    || (record.upstreamStatusCode != null && record.upstreamStatusCode >= 400)
    || record.upstreamError?.message || record.upstreamError?.code || record.upstreamError?.errorType
    || record.continuationUnavailableReason === 'upstream_rejected'
    || record.continuationUnavailableReason === 'quota_exhausted'
    || record.errorKind === 'upstream_capacity_unavailable') {
    kind = 'upstream'
  }
  else if ([
    'scope_unavailable',
    'pool_unavailable',
    'fresh_connection_required',
    'connection_busy',
    'latest_response_mismatch',
    'reused_connection_lost',
  ].includes(record.continuationUnavailableReason ?? '') || [
    'process_interrupted',
    'process_terminated',
    'no_available_provider',
    'no_eligible_account',
    'account_capacity_unavailable',
    'middleware_capacity_unavailable',
    'concurrency_queue_full',
    'concurrency_queue_timeout',
    'provider_infrastructure_unavailable',
    'session_policy_restricted',
    'request_policy_denied',
    'policy_denied',
    'budget_exhausted',
  ].includes(record.errorKind ?? '')) {
    kind = 'proxy'
  }
  else if (record.upstreamConnectionExitReason === 'upstream_close'
    || record.upstreamConnectionExitReason === 'normal_close') {
    kind = 'upstream'
  }
  else if (record.errorKind === 'transport' || [
    'eof',
    'unexpected_eof',
    'transport_error',
    'connection_reset',
    'tcp_reset',
    'reset_without_closing_handshake',
    'pong_timeout',
    'ping_write_timeout',
    'pong_write_timeout',
  ].includes(record.upstreamConnectionExitReason ?? '')) {
    kind = 'connection'
  }

  const origins = {
    proxy: {
      label: t('usage.error.origin.proxy'),
      className: 'bg-z-blue-container text-z-blue-on-container',
      hint: null,
    },
    upstream: {
      label: t('usage.error.origin.upstream'),
      className: 'bg-z-error-container text-z-error-on-container',
      hint: null,
    },
    connection: {
      label: t('usage.error.origin.connection'),
      className: 'bg-z-warning-container text-z-warning-on-container',
      hint: t('usage.error.origin.connectionHint'),
    },
    unknown: {
      label: t('usage.error.origin.unknown'),
      className: 'bg-z-fill-tertiary text-z-text-secondary',
      hint: t('usage.error.origin.unknownHint'),
    },
  }
  return { kind, ...origins[kind] }
}

export function usageSendStateText(state: string) {
  switch (state) {
    case 'not_sent': return t('usage.error.sendState.notSent')
    case 'sent': return t('usage.error.sendState.sent')
    case 'ambiguous': return t('usage.error.sendState.ambiguous')
    default: return state
  }
}

export function usageErrorCode(record: UsageErrorFacts): string | null {
  return record.websocketCloseCode != null
    ? `WS ${record.websocketCloseCode}`
    : record.errorCode
}
