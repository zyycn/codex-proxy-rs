import type { RequestOptions } from '../request'
import type { RelativeTimeView } from '../time'
import type { AccountQuotaWindow, AccountUpstream } from './accounts'
import type { AnalyticsMetadata } from './analytics'
import type { UsageListRecord } from './usage'
import request from '../request'

export type DashboardTrendKind = 'usage' | 'latency' | 'errors'

export type DashboardHealthStatus
  = 'future' | 'no_data' | 'unavailable' | 'unstable' | 'low_sample' | 'stable'

export interface DashboardTrendPoint {
  time: string
  bucket: string
  label: string
  requests: string
  requestsValue: number
  inputTokens: string
  inputTokensValue: number
  outputTokens: string
  outputTokensValue: number
  cachedInputTokens: string
  cachedInputTokensValue: number
  cacheHitRateValue: number
  tokensValue: number
  errors: string
  errorsValue: number
  latencyValue: number | null
  successRate: string
  successRateValue: number | null
  firstTokenP95Ms: number | null
  latencyP95Ms: number | null
  outputThroughputP50: number | null
}

export type DashboardTrendMetric
  = 'inputTokens' | 'outputTokens' | 'cachedInputTokens'
    | 'firstTokenP95Ms' | 'latencyP95Ms' | 'outputThroughputP50'
    | 'errors' | 'successRate' | 'requests'

export interface DashboardTrendSummaryItem {
  metric: DashboardTrendMetric
  value: string
  ratio: string | null
}

export interface DashboardTrendResponse {
  analytics: AnalyticsMetadata
  kind: DashboardTrendKind
  points: DashboardTrendPoint[]
  summary: DashboardTrendSummaryItem[]
}

export interface DashboardCards {
  credentials: {
    total: string
    totalValue: number
    available: string
    availableValue: number
    unavailable: string
    unavailableValue: number
  }
  traffic: {
    todayRequests: string
    todayRequestsValue: number
    yesterdayRequestsValue: number
    totalRequests: string
  }
  tokens: {
    todayTokens: string
    todayTokensValue: number
    yesterdayTokensValue: number
    totalTokens: string
    totalBillingAmountUsd: string
  }
  cache: {
    todayHitRate: string
    todayHitRateValue: number | null
    yesterdayHitRateValue: number | null
    totalHitRate: string
    totalCachedInputTokens: string
    averageFirstTokenLatencyMs: string
  }
}

export interface DashboardHealthTimelinePoint {
  bucketStart: string
  time: string
  status: DashboardHealthStatus
  reliabilityDisplay: string
  successRequests: number
  failedRequests: number
  cancelledRequests: number
  incompleteRequests: number
  callerErrorRequests: number
}

export interface DashboardHealthTimeline {
  title: string
  description: string
  reliabilityDisplay: string
  status: DashboardHealthStatus
  successRequests: number
  failedRequests: number
  cancelledRequests: number
  incompleteRequests: number
  callerErrorRequests: number
  points: DashboardHealthTimelinePoint[]
}

export interface DashboardWireProfile {
  provider: string
  product: string
  version: string
  build?: string
  target: {
    osType: string
    osVersion: string
    arch: string
    terminal: string
  }
  userAgent: string
  attributes: Array<{ key: string, value: string }>
  verifiedAt?: string
  verifiedAtDisplay?: string
  release?: {
    status: 'unchecked' | 'aligned' | 'review_required' | 'check_failed'
    checkedAt?: string
    checkedAtDisplay?: string
    latestVersion?: string
    latestBuild?: string
    error?: string
  }
}

export interface DashboardAccountRequestBucket {
  bucketStart: string
  label: string
  requestCount: number
}

export interface DashboardAccountUsage {
  upstream: AccountUpstream | null
  id: string
  provider: string
  authenticationKind: string
  email: string
  notes: string | null
  planType: string | null
  planTypeDisplay: string
  tokens: string
  requestCount: number
  requestBuckets: DashboardAccountRequestBucket[]
  quotaUsedPercent: number | null
  usageWindow: AccountQuotaWindow | null
  metric: 'requests' | 'tokens'
  metricValueRaw: number | null
  metricLabel: string
  metricValue: string
  lastUsedAt: string | null
  lastUsedRelative: RelativeTimeView | null
  lastUsed: string
}

export interface DashboardPoolSummary {
  total: number
  normal: number
  quotaExhausted: number
  rateLimited: number
  disabled: number
  error: number
}

export interface DashboardCapacityInfo {
  maxConcurrentPerAccount: number
  totalSlots: number | null
  usedSlots: number | null
  availableSlots: number | null
}

export interface DashboardSummaryResponse {
  analytics: AnalyticsMetadata
  asOf: string
  asOfDisplay: string
  cards: DashboardCards
  trend: DashboardTrendResponse
  healthTimeline: DashboardHealthTimeline
  wireProfiles: DashboardWireProfile[]
  accountUsage: DashboardAccountUsage[]
  usageRecords: UsageListRecord[]
  poolSummary: DashboardPoolSummary
  capacityInfo: DashboardCapacityInfo
  rotationStrategy: string
}

interface DashboardQuery {
  period: 'today'
  asOf: number
  kind: string
}

export function getDashboardSummary(data: DashboardQuery, options: RequestOptions = {}) {
  return request<DashboardSummaryResponse>({
    url: '/api/admin/dashboard/summary',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getDashboardTrend(data: DashboardQuery, options: RequestOptions = {}) {
  return request<DashboardTrendResponse>({
    url: '/api/admin/dashboard/trend',
    method: 'GET',
    params: data,
    ...options,
  })
}
