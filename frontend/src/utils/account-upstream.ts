import type { AccountUpstream } from '@/api'
import { t } from '@/i18n'

export function accountUpstreamErrorMessage(reason: AccountUpstream['lastError']): string {
  if (!reason)
    return ''
  return {
    unauthorized: t('accounts.upstream.unauthorized'),
    unsupported: t('accounts.upstream.unsupported'),
    rate_limited: t('accounts.upstream.rateLimited'),
    invalid_response: t('accounts.upstream.invalidResponse'),
    unavailable: t('accounts.upstream.unavailable'),
  }[reason]
}
