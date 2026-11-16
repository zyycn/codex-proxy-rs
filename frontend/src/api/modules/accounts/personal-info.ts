import type { RequestOptions } from '../../request'
import type { AccountIdParam } from './types'
import { API_BASE_URL } from '../../constants'
import request from '../../request'

export interface AccountProfileStatisticsSummary {
  totalTextTokens: number | null
  peakTokens: number | null
  longestTaskDurationMs: number | null
  currentStreakDays: number | null
  longestStreakDays: number | null
}

export interface AccountProfileDailyUsage {
  date: string
  tokens: number
}

export interface AccountProfileInvocation {
  type: string
  pluginId: string | null
  pluginName: string | null
  skillId: string | null
  skillName: string | null
  usageCount: number | null
}

export interface AccountProfileActivityInsights {
  fastModePercent: number | null
  reasoningEffort: string | null
  reasoningEffortPercent: number | null
  skillsExplored: number | null
  totalSkillsUsed: number | null
  totalThreads: number | null
  invocations: AccountProfileInvocation[] | null
}

export interface AccountSubscription {
  startsAt: string | null
  startsAtDisplay: string | null
  expiresAt: string
  expiresAtDisplay: string
  willRenew: boolean | null
  billingPeriod: string | null
  billingCurrency: string | null
  observedAt: string
  observedAtDisplay: string
}

export interface ProfileActivityCalendar {
  rangeLabel: string
  weeks: Array<{ key: string, monthLabel: string | null, cells: Array<{ date: string, dateDisplay: string, tokens: number, isFuture: boolean }> }>
}

export interface AccountProfileStatisticsResponse {
  activityCalendar: ProfileActivityCalendar | null
  displayName: string | null
  username: string | null
  imageUrl: string | null
  hasStatsError: boolean
  summary: AccountProfileStatisticsSummary
  dailyUsage: AccountProfileDailyUsage[] | null
  activityInsights: AccountProfileActivityInsights
}

export interface AccountPersonalInfoResponse {
  profile: AccountProfileStatisticsResponse | null
  profileError: string | null
  subscription: AccountSubscription | null
}

export function getAccountPersonalInfo(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountPersonalInfoResponse>({
    url: '/api/admin/accounts/personal-info',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function accountProfileAvatarUrl(accountId: string, sourceUrl: string) {
  const params = new URLSearchParams({
    accountId,
    version: stableAvatarVersion(sourceUrl),
  })
  return `${API_BASE_URL}/api/admin/accounts/profile-avatar?${params.toString()}`
}

function stableAvatarVersion(value: string) {
  let hash = 0
  for (const character of value)
    hash = (hash * 33 + (character.codePointAt(0) ?? 0)) % 2_147_483_647
  return hash.toString(36)
}
