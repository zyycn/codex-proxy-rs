import type { Component } from 'vue'
import type { AccountErrorReason, AccountStatus } from '@/api'
import { AlertTriangle, CircleCheck, Gauge, Power, Timer } from '@lucide/vue'
import { computed } from 'vue'
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
  nextRefreshDisplay: string | null
  rateLimitRecovery: string | null
  recoveryTimeLabel: string
  triggerLabel: string
}

interface AccountStatusPresentationInput {
  status: AccountStatus
  errorReason: AccountErrorReason | null
  errorMessage: string | null
  rateLimitRecoveryDisplay: string | null
  rateLimitReason: 'upstream_rate_limit' | 'capacity_freeze' | null
  recoveryProbeRequired: boolean
  nextRefreshAt: string | null
  nextRefreshAtDisplay: string | null
  now: number
}

const statusStyles: Record<AccountStatusTone, AccountStatusStyle> = {
  success: {
    text: 'text-cp-success-text',
    dot: 'bg-cp-success',
    badge: 'bg-cp-success-container text-cp-success-on-container',
    icon: 'bg-cp-success-container text-cp-success-on-container',
  },
  danger: {
    text: 'text-cp-error-text',
    dot: 'bg-cp-error',
    badge: 'bg-cp-error-container text-cp-error-on-container',
    icon: 'bg-cp-error-container text-cp-error-on-container',
  },
  warning: {
    text: 'text-cp-warning-text',
    dot: 'bg-cp-warning',
    badge: 'bg-cp-warning-container text-cp-warning-on-container',
    icon: 'bg-cp-warning-container text-cp-warning-on-container',
  },
  info: {
    text: 'text-cp-info-text',
    dot: 'bg-cp-info',
    badge: 'bg-cp-info-container text-cp-info-on-container',
    icon: 'bg-cp-info-container text-cp-info-on-container',
  },
  normal: {
    text: 'text-cp-text-secondary',
    dot: 'bg-cp-text-quaternary',
    badge: 'bg-cp-fill-quaternary text-cp-text-secondary',
    icon: 'bg-cp-fill-quaternary text-cp-text-secondary',
  },
}

const displayDefinitions = computed<Record<AccountStatusDisplayMode, AccountStatusDisplayDefinition>>(() => ({
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
  const mode: AccountStatusDisplayMode = isBackoff ? 'refresh_backoff' : input.status
  const isFreeze = mode === 'rate_limited' && input.rateLimitReason === 'capacity_freeze'
  const waitsForProbe = isFreeze && input.recoveryProbeRequired
  const definition = isFreeze
    ? {
        ...displayDefinitions.value.rate_limited,
        description: t('accounts.status.capacityFailuresHelp'),
        recoveryHint: waitsForProbe
          ? t('accounts.status.recoveryProbeHelp')
          : t('accounts.status.recoveryHelp'),
      }
    : displayDefinitions.value[mode]
  const nextRefreshDisplay = isBackoff ? input.nextRefreshAtDisplay : null
  const reasonLabel = input.errorReason ? errorReasonLabels.value[input.errorReason] : null
  const title = definition.title ?? reasonLabel ?? definition.label
  const rateLimitRecovery = mode === 'rate_limited'
    ? input.rateLimitRecoveryDisplay
    : null
  const recoveryHint = mode !== 'refresh_backoff'
    && mode !== 'rate_limited'
    && input.errorReason
    ? errorRecoveryHints.value[input.errorReason]
    : definition.recoveryHint
  const hasDetail = input.status === 'error' || mode === 'rate_limited' || isBackoff
  const retry = nextRefreshDisplay
    ? t('accounts.status.nextAttempt', { nextRefreshDisplay })
    : ''
  const triggerLabel = t('accounts.status.clickOrFocusToViewDetails', { title, description: definition.description, retry })

  return {
    mode,
    statusStyle: statusStyles[definition.tone],
    label: definition.label,
    title,
    description: definition.description,
    recoveryHint,
    icon: definition.icon,
    hasDetail,
    errorText: input.errorMessage || null,
    nextRefreshDisplay,
    rateLimitRecovery,
    recoveryTimeLabel: waitsForProbe
      ? t('accounts.status.resumeDetection')
      : t('accounts.status.expectedToRecover'),
    triggerLabel,
  }
}
