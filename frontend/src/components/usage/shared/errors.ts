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

export function usageErrorCode(record: UsageErrorFacts): string | null {
  return record.websocketCloseCode != null
    ? `WS ${record.websocketCloseCode}`
    : record.errorCode
}
