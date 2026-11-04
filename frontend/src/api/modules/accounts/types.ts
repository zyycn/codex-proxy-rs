import type { RelativeTimeView } from '../../types/time'
import type { AccountGroupRef } from '../groups'

export type AccountStatus
  = 'normal' | 'quota_exhausted' | 'rate_limited' | 'disabled' | 'error'

export type AccountErrorReason
  = 'account_unverified'
    | 'access_token_expired'
    | 'credential_expired'
    | 'credential_invalid'
    | 'account_banned'

export interface AccountQuotaWindow {
  sourceAccountId?: string | null
  amount?: { usedDisplay: string, remainingDisplay: string, totalDisplay: string } | null
  observedAtDisplay?: string | null
  stale?: boolean
  key: string
  group: string
  limitId: string | null
  limitName: string | null
  role: 'primary' | 'secondary' | 'monthly' | null
  windowSeconds: number | null
  labelDisplay: string
  windowLabelDisplay: string
  usedPercent: number | null
  usedPercentDisplay: string
  limitReached: boolean
  localUsage?: unknown
  resetAtDisplay: string
}

export interface AccountQuotaCredits {
  hasCredits: boolean
  unlimited: boolean
  balance: string | null
}

export interface AccountProtectionRule {
  id: string
  name: string
}

export interface AccountQuota {
  refreshedAt: string | null
  refreshedAtRelative: RelativeTimeView | null
  refreshedAtDisplay: string
  limitReached: boolean
  // 429 临时限流（Redis 冷却）到期时间；非限流中为 null。
  rateLimitedUntil: string | null
  rateLimitRecoveryDisplay: string | null
  rateLimitReason: 'upstream_rate_limit' | 'error_protection' | null
  protectionRules: AccountProtectionRule[]
  recoveryProbeRequired: boolean
  windows: AccountQuotaWindow[]
  credits: AccountQuotaCredits | null
}

export interface AccountCurrencyCost {
  currency: string
  estimatedAmount: string
  estimatedAmountDisplay: string
}

export interface AccountModelUsage {
  model: string
  requestCount: number
  requestCountDisplay: string
  successRate: number | null
  successRateDisplay: string
  inputTokens: number | null
  inputTokensDisplay: string
  outputTokens: number | null
  outputTokensDisplay: string
  cachedInputTokens: number | null
  cachedInputTokensDisplay: string
  imageInputTokens: number | null
  imageInputTokensDisplay: string
  imageOutputTokens: number | null
  imageOutputTokensDisplay: string
  imageRequestCount: number
  imageRequestCountDisplay: string
  imageRequestFailedCount: number
  imageRequestFailedCountDisplay: string
  totalTokens: number | null
  totalTokensDisplay: string
  billingAmountUsd: string | null
  billingAmountUsdDisplay: string
  costEstimateStatus: string
  knownCostCount: number
  partialCostCount: number
  unknownCostCount: number
  costs: AccountCurrencyCost[]
  lastUsedAt: string
  lastUsedAtDisplay: string
  lastUsedAtFullDisplay: string | null
}

export interface AccountUsage {
  windowKind: 'general' | 'daily' | 'weekly' | 'monthly' | 'quota'
  requestCount: number | null
  requestCountDisplay: string
  inputTokens: number | null
  inputTokensDisplay: string
  outputTokens: number | null
  outputTokensDisplay: string
  cachedInputTokens: number | null
  cachedInputTokensDisplay: string
  reasoningOutputTokens: number | null
  reasoningOutputTokensDisplay: string
  imageInputTokens: number | null
  imageInputTokensDisplay: string
  imageOutputTokens: number | null
  imageOutputTokensDisplay: string
  imageRequestCount: number | null
  imageRequestCountDisplay: string
  imageRequestFailedCount: number | null
  imageRequestFailedCountDisplay: string
  totalTokens: number | null
  totalTokensDisplay: string
  cacheWriteInputTokens: number | null
  cacheWriteInputTokensDisplay: string
  lastUsedAt: string | null
  lastUsedAtDisplay: string
  lastUsedAtFullDisplay: string | null
  costEstimateStatus: string
  knownCostCount: number | null
  partialCostCount: number | null
  unknownCostCount: number | null
  costs: AccountCurrencyCost[]
  models: AccountModelUsage[]
}

export interface AccountModelAccess {
  mode: 'all' | 'allowlist' | 'denylist'
  models: string[]
}

export interface Account {
  upstream: AccountUpstream | null
  capabilities: AccountCapabilities
  outboundProxyId: string | null
  outboundProxyEndpoint: string | null
  id: string
  name: string
  notes: string | null
  provider: string
  resourceRef: string
  email: string | null
  accountId: string | null
  userId: string | null
  label: string | null
  planType: string | null
  planTypeDisplay: string
  authenticationKind: string
  hasRefreshToken: boolean
  status: AccountStatus
  errorReason: AccountErrorReason | null
  errorMessage: string | null
  enabled: boolean
  concurrencyLimit: number | null
  capacity: {
    usedSlots: number | null
    totalSlots: number | null
  }
  weight: number
  modelAccess: AccountModelAccess
  accessTokenExpiresAt: string | null
  accessTokenExpiresAtDisplay: string | null
  refreshTokenExpiresAt: string | null
  nextRefreshAt: string | null
  nextRefreshAtDisplay: string | null
  addedAt: string
  addedAtDisplay: string
  updatedAt: string
  updatedAtDisplay: string
  quota: AccountQuota
  usage: AccountUsage
  groups: AccountGroupRef[]
}

export interface AccountUpstream {
  // 明确关闭共享或不支持互联时为 false，普通鉴权失败保留已识别身份
  discovered: boolean
  observedAt: string | null
  observedAtDisplay: string
  observedAtRelative: RelativeTimeView | null
  stale: boolean
  connectionRevision: string
  lastError: 'unauthorized' | 'sharing_disabled' | 'unsupported' | 'rate_limited' | 'invalid_response' | 'unavailable' | null
  maxConcurrency: number | null
  requestsPerMinute: number | null
  pool: {
    accountCount: number
    states: { ready: number, cooldown: number, exhausted: number, disabled: number, error: number, unknown: number }
    plans: Array<{ name: string, count: number }>
  } | null
}

export interface AccountCapabilities {
  quota: boolean
  quotaRefresh: boolean
  profile: boolean
  subscription: boolean
  avatar: boolean
  resetCredits: boolean
  consumeResetCredit: boolean
}

export interface AccountRefreshResponse {
  account: Account
  result?: string
  error?: string
}

export interface AccountIdParam {
  accountId: string
}
