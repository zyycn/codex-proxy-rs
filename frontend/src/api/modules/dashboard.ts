import type { UsageRecord } from './usage'
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
  cachedTokens: string
  cachedTokensValue: number
  cacheHitRateValue: number
  tokensValue: number
  errors: string
  errorsValue: number
  latency: string
  latencyValue: number | null
  maxLatency: string
  maxLatencyValue: number | null
  minLatency: string
  minLatencyValue: number | null
  successRate: string
  successRateValue: number | null
}

export interface DashboardTrendSummaryItem {
  label: string
  value: string
  ratio: string | null
}

export interface DashboardTrendResponse {
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
    totalCachedTokens: string
    averageFirstTokenLatencyMs: string
  }
}

export interface DashboardHealthTimelinePoint {
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
  attributes: Array<{ label: string, value: string }>
  verifiedAt?: string
  release?: {
    status: 'unchecked' | 'aligned' | 'review_required' | 'check_failed'
    checkedAt?: string
    latestVersion?: string
    latestBuild?: string
    error?: string
  }
}

export interface DashboardAccountRequestBucket {
  bucketStart: string
  requestCount: number
}

export interface DashboardAccountUsage {
  id: string
  provider: string
  authenticationKind: string
  email: string
  planType: string | null
  tokens: string
  requestCount: number
  requestBuckets: DashboardAccountRequestBucket[]
  quotaUsedPercent: number | null
  lastUsed: string
}

export interface DashboardPoolSummary {
  total: number
  active: number
  expired: number
  quotaExhausted: number
  refreshing: number | null
  disabled: number
  banned: number
}

export interface DashboardCapacityInfo {
  maxConcurrentPerAccount: number
  totalSlots: number
  usedSlots: number | null
  availableSlots: number | null
}

export interface DashboardSummaryResponse {
  cards: DashboardCards
  trend: DashboardTrendResponse
  healthTimeline: DashboardHealthTimeline
  wireProfiles: DashboardWireProfile[]
  accountUsage: DashboardAccountUsage[]
  usageRecords: UsageRecord[]
  poolSummary: DashboardPoolSummary
  capacityInfo: DashboardCapacityInfo
  rotationStrategy: string
}

export function getDashboardSummary(data: object) {
  return request<DashboardSummaryResponse>({
    url: '/api/admin/dashboard/summary',
    method: 'GET',
    params: data,
  })
}

export function getDashboardTrend(data: object) {
  return request<DashboardTrendResponse>({
    url: '/api/admin/dashboard/trend',
    method: 'GET',
    params: data,
  })
}
