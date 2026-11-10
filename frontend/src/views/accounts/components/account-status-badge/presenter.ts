import type { Component } from 'vue'
import type { AccountErrorReason, AccountProtectionRule, AccountQuota, AccountStatus } from '@/api'
import { AlertTriangle, CircleCheck, Gauge, Power, Timer } from '@lucide/vue'
import { computed } from 'vue'
import { quotaWindowLabel } from '@/components/account/account-usage-window/presenter'
import { t } from '@/i18n'

import { errorReasonLabels, statusLabels, statusTones } from '../../constants'

type AccountStatusDisplayMode = AccountStatus | 'refresh_backoff'

type AccountStatusTone = (typeof statusTones)[AccountStatus]

interface AccountStatusDisplayDefinition {
  tone: AccountStatusTone
  label: string
  title?: string
  description: string
  recoveryHint: string
  icon: Component
}

interface AccountStatusStyle {
  text: string
  dot: string
  badge: string
  icon: string
}

interface AccountStatusPresentation {
  mode: AccountStatusDisplayMode
  statusStyle: AccountStatusStyle
  label: string
  title: string
  description: string
  recoveryHint: string
  icon: Component
  hasDetail: boolean
  errorText: string | null
  times: { key: string, label: string, value: string }[]
  protectionRules: AccountProtectionRule[]
  triggerLabel: string
}

interface AccountStatusPresentationInput {
  quota?: AccountQuota
  status: AccountStatus
  errorReason: AccountErrorReason | null
  errorMessage: string | null
  rateLimitRecoveryDisplay: string | null
  rateLimitReason: 'upstream_rate_limit' | 'error_protection' | null
  protectionRules: AccountProtectionRule[]
  recoveryProbeRequired: boolean
  nextRefreshAt: string | null
  nextRefreshAtDisplay: string | null
  now: number
}

const statusStyles: Record<AccountStatusTone, AccountStatusStyle> = {
  success: {
    text: 'text-z-success-text',
    dot: 'bg-z-success',
    badge: 'bg-z-success-container text-z-success-on-container',
    icon: 'bg-z-success-container text-z-success-on-container',
  },
  danger: {
    text: 'text-z-error-text',
    dot: 'bg-z-error',
    badge: 'bg-z-error-container text-z-error-on-container',
    icon: 'bg-z-error-container text-z-error-on-container',
  },
  warning: {
    text: 'text-z-warning-text',
    dot: 'bg-z-warning',
    badge: 'bg-z-warning-container text-z-warning-on-container',
    icon: 'bg-z-warning-container text-z-warning-on-container',
  },
  info: {
    text: 'text-z-info-text',
    dot: 'bg-z-info',
    badge: 'bg-z-info-container text-z-info-on-container',
    icon: 'bg-z-info-container text-z-info-on-container',
  },
  normal: {
    text: 'text-z-text-secondary',
    dot: 'bg-z-text-quaternary',
    badge: 'bg-z-fill-quaternary text-z-text-secondary',
    icon: 'bg-z-fill-quaternary text-z-text-secondary',
  },
}

const displayDefinitions = computed<Record<AccountStatusDisplayMode, AccountStatusDisplayDefinition>>(() => ({
  quota_paused: {
    tone: statusTones.quota_paused,
    label: statusLabels.value.quota_paused,
    description: t('accounts.quotaLimits.reached', {
      windows: t('accounts.quota.general'),
    }),
    recoveryHint: t('accounts.quotaLimits.recovery'),
    icon: Gauge,
  },
  refresh_backoff: {
    tone: 'warning',
    label: t('accounts.status.retreating'),
    title: t('accounts.status.oauthRefreshBackoff'),
    description: t('accounts.status.refreshFailedHelp'),
    recoveryHint: t('accounts.status.retryScheduledHelp'),
    icon: Timer,
  },
  normal: {
    tone: statusTones.normal,
    label: statusLabels.value.normal,
    description: t('accounts.status.attentionHelp'),
    recoveryHint: t('accounts.status.retestHelp'),
    icon: CircleCheck,
  },
  quota_exhausted: {
    tone: statusTones.quota_exhausted,
    label: statusLabels.value.quota_exhausted,
    description: t('accounts.status.attentionHelp'),
    recoveryHint: t('accounts.status.retestHelp'),
    icon: Gauge,
  },
  rate_limited: {
    tone: statusTones.rate_limited,
    label: statusLabels.value.rate_limited,
    description: t('accounts.status.rateLimitedHelp'),
    recoveryHint: t('accounts.status.cooldownHelp'),
    icon: Timer,
  },
  disabled: {
    tone: statusTones.disabled,
    label: statusLabels.value.disabled,
    description: t('accounts.status.attentionHelp'),
    recoveryHint: t('accounts.status.retestHelp'),
    icon: Power,
  },
  error: {
    tone: statusTones.error,
    label: statusLabels.value.error,
    description: t('accounts.status.credentialsHelp'),
    recoveryHint: t('accounts.status.retestHelp'),
    icon: AlertTriangle,
  },
}))

const errorRecoveryHints = computed<Record<AccountErrorReason, string>>(() => ({
  account_unverified: t('accounts.status.reauthorizeHelp'),
  access_token_expired: t('accounts.status.reauthorizeHelp'),
  credential_expired: t('accounts.status.reauthorizeHelp'),
  credential_invalid: t('accounts.status.replaceCredentialsHelp'),
  account_banned: t('accounts.status.upstreamRestrictionHelp'),
}))

export function resolveAccountStatusPresentation(
  input: AccountStatusPresentationInput,
): AccountStatusPresentation {
  const isBackoff = input.status !== 'disabled'
    && input.nextRefreshAt !== null
    && Date.parse(input.nextRefreshAt) > input.now
  const mode: AccountStatusDisplayMode = isBackoff && input.status !== 'quota_paused'
    ? 'refresh_backoff'
    : input.status
  const quota = input.quota
  const pausedWindows = mode === 'quota_paused' && quota
    ? quota.pausedWindows.map((key) => {
        const window = quota.windows.find(w => w.key === key)
        return {
          key,
          label: window
            ? quotaWindowLabel(window)
            : key,
          reset: window?.resetAtDisplay,
        }
      })
    : []
  const isProtected = mode === 'rate_limited' && input.rateLimitReason === 'error_protection'
  const waitsForProbe = isProtected && input.recoveryProbeRequired
  const definition = isProtected
    ? {
        ...displayDefinitions.value.rate_limited,
        label: t('accounts.status.errorProtection'),
        description: t('accounts.status.protectionFailuresHelp'),
        recoveryHint: waitsForProbe
          ? t('accounts.status.recoveryProbeHelp')
          : t('accounts.status.recoveryHelp'),
      }
    : displayDefinitions.value[mode]
  const nextRefreshDisplay = isBackoff ? input.nextRefreshAtDisplay : null
  const reasonLabel = input.errorReason ? errorReasonLabels.value[input.errorReason] : null
  const title = definition.title ?? reasonLabel ?? definition.label
  const description = pausedWindows.length
    ? t('accounts.quotaLimits.reached', {
        windows: pausedWindows.map(w => w.label).join(' / '),
      })
    : definition.description
  const times: AccountStatusPresentation['times'] = []
  if (mode === 'rate_limited' && input.rateLimitRecoveryDisplay) {
    times.push({
      key: 'cooldown',
      label: waitsForProbe
        ? t('accounts.status.resumeDetection')
        : t('accounts.status.expectedToRecover'),
      value: input.rateLimitRecoveryDisplay,
    })
  }
  for (const window of pausedWindows) {
    if (window.reset && window.reset !== '—') {
      times.push({
        key: window.key,
        label: pausedWindows.length === 1
          ? t('accounts.quotaLimits.expectedReset')
          : t('accounts.quotaLimits.windowReset', { window: window.label }),
        value: window.reset,
      })
    }
  }
  if (nextRefreshDisplay) {
    times.push({
      key: 'refresh',
      label: t('accounts.status.tryNextTime'),
      value: nextRefreshDisplay,
    })
  }
  const recoveryHint = mode !== 'refresh_backoff'
    && mode !== 'rate_limited'
    && input.errorReason
    ? errorRecoveryHints.value[input.errorReason]
    : definition.recoveryHint
  const hasDetail = input.status === 'error' || mode === 'rate_limited' || isBackoff || mode === 'quota_paused'
  const retry = nextRefreshDisplay
    ? t('accounts.status.nextAttempt', { nextRefreshDisplay })
    : ''
  const triggerLabel = t('accounts.status.clickOrFocusToViewDetails', { title, description, retry })

  return {
    mode,
    statusStyle: statusStyles[definition.tone],
    label: definition.label,
    title,
    description,
    recoveryHint,
    icon: definition.icon,
    hasDetail,
    errorText: input.errorMessage || null,
    times,
    protectionRules: isProtected
      ? input.protectionRules
      : [],
    triggerLabel,
  }
}
