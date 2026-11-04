import type { UsageErrorFacts } from '@/api/modules/usage'
import { t } from '@/i18n'

export type UsageErrorRecord = UsageErrorFacts & {
  provider: string | null
  logicalOutcome: string
}

// 只解释已记录的分类和关闭码，原始上游文案由详情保留
export function usageErrorSummary(record: UsageErrorRecord): string | null {
  if (record.websocketCloseCode != null) {
    if (record.websocketCloseCode === 1012)
      return t('usage.error.summary.upstreamRestart')
    if (record.websocketCloseCode === 1013)
      return t('usage.error.summary.upstreamBusy')
    if (record.websocketCloseCode === 1009)
      return t('usage.error.summary.messageTooLarge')
    return t('usage.error.summary.upstreamClosed')
  }

  if (record.errorKind === 'process_interrupted')
    return t('usage.error.summary.processInterrupted')

  if (record.errorKind === 'continuation_recovery_required') {
    if (record.continuationUnavailableReason === 'reused_connection_lost'
      && record.upstreamConnectionExitReason === 'max_age_expired') {
      return t('usage.error.summary.continuationExpired')
    }
    return t('usage.error.summary.continuationUnavailable')
  }

  if (record.provider === 'openai') {
    switch (record.errorCode) {
      case 'previous_response_not_found':
        return t('usage.error.summary.continuationUnavailable')
      case 'websocket_connection_limit_reached':
        return t('usage.error.summary.connectionLimit')
      case 'context_length_exceeded':
        return t('usage.error.summary.contextTooLong')
      case 'insufficient_quota':
        return t('usage.error.summary.quotaExceeded')
      case 'rate_limit_exceeded':
        return t('usage.error.summary.rateLimited')
      case 'server_is_overloaded':
        return t('usage.error.summary.serverOverloaded')
    }
  }

  if (record.upstreamConnectionExitReason === 'upstream_close')
    return t('usage.error.summary.upstreamClosed')
  return null
}

export function usageErrorCode(record: UsageErrorFacts): string | null {
  return record.websocketCloseCode != null
    ? `WS ${record.websocketCloseCode}`
    : record.errorCode
}
