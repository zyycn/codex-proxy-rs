import { requestJson } from '../request'

export type DashboardTrendKind = 'usage' | 'latency' | 'errors'

export interface DashboardSummary {
  cards: DashboardCards
  trend: DashboardTrend
  healthTimeline: DashboardHealthTimeline
  accountUsage: DashboardAccountUsage[]
  serviceStatuses: DashboardServiceStatus[]
  eventLogs: DashboardEventLog[]
  poolSummary: DashboardAccountPoolSummary
  capacityInfo: DashboardAccountCapacityInfo
  rotationStrategy?: string
}

export interface DashboardHealthTimeline {
  title: string
  description: string
  rangeDisplay: string
  reliabilityDisplay: string
  oldestLabel: string
  newestLabel: string
  points: string
}

export interface DashboardCards {
  accounts: DashboardAccountsCard
  traffic: DashboardTrafficCard
  tokens: DashboardTokenCard
  cache: DashboardCacheCard
}

export interface DashboardAccountsCard {
  total: number
  enabled: number
  abnormal: number
}

export interface DashboardTrafficCard {
  todayRequests: number
  yesterdayRequests: number
  totalRequests: number
  rpm: number
  tpm: number
}

export interface DashboardTokenCard {
  todayTokens: number
  yesterdayTokens: number
  totalTokens: number
  todayCostUsd?: number | null
  totalCostUsd?: number | null
}

export interface DashboardCacheCard {
  todayHitRate?: number | null
  yesterdayHitRate?: number | null
  totalHitRate?: number | null
  totalCachedTokens: number
  firstTokenLatencyMs?: number | null
  completionLatencyMs?: number | null
}

export interface DashboardTrend {
  kind: DashboardTrendKind
  points: DashboardTrendPoint[]
  summary: DashboardTrendSummary[]
}

export interface DashboardTrendPoint {
  time: string
  requests: number
  inputTokens: number
  outputTokens: number
  cachedTokens: number
  tokens: number
  errors: number
  latency: number
  maxLatency: number
  minLatency: number
  successRate: number
}

export interface DashboardTrendSummary {
  label: string
  value: number
  ratio?: number | null
}

export interface DashboardAccountUsage {
  name: string
  email: string
  plan: string
  requests: number
  tokens: number
  quotaUsedPercent?: number | null
  lastUsed: string
  status: 'active' | 'expired' | 'disabled' | 'banned' | 'quota_exhausted' | 'refreshing'
}

export interface DashboardEventLog {
  id: string
  time: string
  level: 'debug' | 'info' | 'warn' | 'error'
  requestId?: string
  route?: string
  model?: string
  statusCode?: number
  latencyMs?: number
}

export interface DashboardServiceStatus {
  label: string
  value: string
  detail: string
  tone: 'normal' | 'info' | 'success' | 'warning' | 'danger'
}

export interface DashboardAccountPoolSummary {
  total: number
  active: number
  expired: number
  quotaExhausted: number
  refreshing: number
  disabled: number
  banned: number
}

export interface DashboardAccountCapacityInfo {
  maxConcurrentPerAccount: number
  totalSlots: number
  usedSlots: number
  availableSlots: number
}

export function getDashboardSummary() {
  return requestJson<DashboardSummary>('/api/admin/dashboard/summary')
}

export function getDashboardTrend(kind: DashboardTrendKind) {
  const params = new URLSearchParams({ kind })
  return requestJson<DashboardTrend>(`/api/admin/dashboard/trend?${params}`)
}
