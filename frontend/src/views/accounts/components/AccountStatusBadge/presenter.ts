import type { Component } from 'vue'
import type { AccountErrorReason, AccountStatus } from '@/api'
import { AlertTriangle, CircleCheck, Gauge, Power, Timer } from '@lucide/vue'

import { formatDateTime, parseTimestamp } from '@/utils/date'
import { errorReasonLabels, statusLabels, statusTones } from '../../constants'

export type AccountStatusDisplayMode = AccountStatus | 'refresh_backoff'

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

export interface AccountStatusPresentation {
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
  triggerLabel: string
}

export interface AccountStatusPresentationInput {
  status: AccountStatus
  errorReason: AccountErrorReason | null
  errorMessage: string | null
  rateLimitedUntil: string | null
  nextRefreshAt: string | null
  now: number
}

const statusStyles: Record<AccountStatusTone, AccountStatusStyle> = {
  success: {
    text: 'text-cp-success-text',
    dot: 'bg-cp-success',
    badge: 'bg-cp-success-bg text-cp-success-text',
    icon: 'bg-cp-success-bg text-cp-success-text',
  },
  danger: {
    text: 'text-cp-danger-text',
    dot: 'bg-cp-danger',
    badge: 'bg-cp-danger-bg text-cp-danger-text',
    icon: 'bg-cp-danger-bg text-cp-danger-text',
  },
  warning: {
    text: 'text-cp-warning-text',
    dot: 'bg-cp-warning',
    badge: 'bg-cp-warning-bg text-cp-warning-text',
    icon: 'bg-cp-warning-bg text-cp-warning-text',
  },
  info: {
    text: 'text-cp-info-text',
    dot: 'bg-cp-info',
    badge: 'bg-cp-info-bg text-cp-info-text',
    icon: 'bg-cp-info-bg text-cp-info-text',
  },
  normal: {
    text: 'text-cp-secondary',
    dot: 'bg-cp-muted-text',
    badge: 'bg-cp-subtle text-cp-secondary',
    icon: 'bg-cp-subtle text-cp-secondary',
  },
}

const displayDefinitions: Record<AccountStatusDisplayMode, AccountStatusDisplayDefinition> = {
  refresh_backoff: {
    tone: 'warning',
    label: '退避中',
    title: 'OAuth 刷新退避',
    description: '刷新失败，系统正在等待下一次自动尝试。',
    recoveryHint: '系统会在计划时间自动重试。',
    icon: Timer,
  },
  normal: {
    tone: statusTones.normal,
    label: statusLabels.normal,
    description: '该账号当前状态需要关注。',
    recoveryHint: '重新测试连接以获取最新状态。',
    icon: CircleCheck,
  },
  quota_exhausted: {
    tone: statusTones.quota_exhausted,
    label: statusLabels.quota_exhausted,
    description: '该账号当前状态需要关注。',
    recoveryHint: '重新测试连接以获取最新状态。',
    icon: Gauge,
  },
  rate_limited: {
    tone: statusTones.rate_limited,
    label: statusLabels.rate_limited,
    description: '上游暂时限制了请求频率，系统正在冷却该账号。',
    recoveryHint: '冷却结束后，系统会自动恢复调度。',
    icon: Timer,
  },
  disabled: {
    tone: statusTones.disabled,
    label: statusLabels.disabled,
    description: '该账号当前状态需要关注。',
    recoveryHint: '重新测试连接以获取最新状态。',
    icon: Power,
  },
  error: {
    tone: statusTones.error,
    label: statusLabels.error,
    description: '该账号暂不参与调度，处理凭据后可重新测试连接。',
    recoveryHint: '重新测试连接以获取最新状态。',
    icon: AlertTriangle,
  },
}

const errorRecoveryHints: Record<AccountErrorReason, string> = {
  account_unverified: '重新授权后，账号会重新参与调度。',
  access_token_expired: '重新授权后，账号会重新参与调度。',
  credential_expired: '重新授权后，账号会重新参与调度。',
  credential_invalid: '请更新或重新导入凭据，再次测试连接。',
  account_banned: '请确认上游账号状态，解除限制后再启用。',
}

export function resolveAccountStatusPresentation(
  input: AccountStatusPresentationInput,
): AccountStatusPresentation {
  const nextRefreshTimestamp = input.nextRefreshAt === null
    ? null
    : parseTimestamp(input.nextRefreshAt)
  const isBackoff = input.status !== 'disabled'
    && nextRefreshTimestamp !== null
    && nextRefreshTimestamp > input.now
  const mode: AccountStatusDisplayMode = isBackoff ? 'refresh_backoff' : input.status
  const definition = displayDefinitions[mode]
  const nextRefreshDisplay = isBackoff ? formatDateTime(nextRefreshTimestamp) : null
  const reasonLabel = input.errorReason ? errorReasonLabels[input.errorReason] : null
  const title = definition.title ?? reasonLabel ?? definition.label
  const rateLimitRecovery = input.status === 'rate_limited'
    ? remainingTime(input.rateLimitedUntil, input.now)
    : null
  const recoveryHint = mode !== 'refresh_backoff'
    && mode !== 'rate_limited'
    && input.errorReason
    ? errorRecoveryHints[input.errorReason]
    : definition.recoveryHint
  const hasDetail = input.status === 'error' || rateLimitRecovery !== null || isBackoff
  const retry = nextRefreshDisplay ? ` 下次尝试：${nextRefreshDisplay}。` : ''
  const triggerLabel = `${title}。${definition.description}${retry} 点击或聚焦查看详情。`

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
    triggerLabel,
  }
}

function remainingTime(value: string | null, now: number) {
  if (!value)
    return null

  const until = parseTimestamp(value)
  if (until === null)
    return value

  const minutes = Math.round((until - now) / 60_000)
  if (minutes < 1)
    return value
  if (minutes < 60)
    return `剩余 ${minutes} 分钟`

  const hours = Math.floor(minutes / 60)
  const remainingMinutes = minutes % 60
  return remainingMinutes === 0
    ? `剩余 ${hours} 小时`
    : `剩余 ${hours} 小时 ${remainingMinutes} 分`
}
