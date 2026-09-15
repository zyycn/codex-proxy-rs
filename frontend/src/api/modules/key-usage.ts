import type { RequestOptions } from '../request'
import type { DashboardHealthTimeline } from './dashboard'
import type { UsageBilling, UsageLatencyDetails, UsageTokenDetails } from './usage'
import request from '../request'

export interface KeyUsageMetrics {
  requests: number
  inputTokens: number
  outputTokens: number
  cachedTokens: number
  cacheWriteTokens: number
  reasoningTokens: number
  totalTokens: number
  costUsd: string | null
  costIncomplete: boolean
}

export interface KeyUsageBudget {
  name: string
  prefix: string
  maxConcurrency: number
  requestsPerMinute: number
  dailyLimitUsd: string
  dailyUsedUsd: string
  dailyResetsAt: string | null
  weeklyLimitUsd: string
  weeklyUsedUsd: string
  weeklyResetsAt: string | null
}

export interface KeyUsageTrendPoint extends KeyUsageMetrics {
  time: string
  bucketSeconds: number
}

export interface KeyUsageOverview {
  asOf: string
  startTime: string
  endTime: string
  key: KeyUsageBudget
  summary: KeyUsageMetrics
  trend: KeyUsageTrendPoint[]
  healthTimeline: DashboardHealthTimeline
}

export type KeyUsageRecordKind = 'success' | 'error'

export interface KeyUsageRecord {
  id: string
  createdAt: string
  model: string | null
  route: string | null
  reasoningEffort: string | null
  clientTransport: string | null
  upstreamTransport: string | null
  tokenDetails: UsageTokenDetails | null
  billing: UsageBilling | null
  latencyMs: number | null
  firstTokenLatencyMs: number | null
  latencyDetails: Pick<UsageLatencyDetails, 'firstEventMs' | 'firstReasoningMs' | 'firstTextMs'>
  clientIp: string | null
  userAgent: string | null
  status: KeyUsageRecordKind
  statusCode: number | null
}

export interface KeyUsagePage {
  items: KeyUsageRecord[]
  currentPage: number
  pageSize: number
  total: number
}

export interface KeyUsageQuery {
  startTime: string
  endTime: string
  model?: string
}

export function getKeyUsageOverview(params: KeyUsageQuery, options: RequestOptions = {}) {
  return request<KeyUsageOverview>({
    url: '/api/key-usage/overview',
    method: 'GET',
    params,
    ...options,
  })
}

export function getKeyUsageRecords(
  params: KeyUsageQuery & { kind: KeyUsageRecordKind, currentPage: number, pageSize: number },
  options: RequestOptions = {},
) {
  return request<KeyUsagePage>({
    url: '/api/key-usage/records',
    method: 'GET',
    params,
    ...options,
  })
}
