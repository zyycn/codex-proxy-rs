import type { RequestOptions } from '../request'
import type { DashboardHealthTimeline, DashboardWireProfile } from './dashboard'
import type {
  UsageBilling,
  UsageDiagnosticsResponse,
  UsageInsightsOverviewResponse,
  UsageLatencyDetails,
  UsageSummaryResponse,
  UsageTokenDetails,
} from './usage'
import request from '../request'

export interface ClientUsageRecord {
  id: string
  route: string
  model: string | null
  requestedModel: string | null
  upstreamModel: string | null
  clientTransport: string
  upstreamTransport: string | null
  reasoningEffort: string | null
  reasoningPreset: string | null
  subagentKind: string | null
  compact: boolean
  tokenDetails: UsageTokenDetails
  billing: UsageBilling | null
  latencyDetails: UsageLatencyDetails
  firstTokenLatencyMs: number | null
  latencyMs: number | null
  createdAt: string
  createdAtDisplay: string
  clientIp: string | null
  userAgent: string | null
}

export interface ClientUsageRecordsResponse {
  items: ClientUsageRecord[]
  currentPage: number
  pageSize: number
  total: number
}

export interface ClientUsageRecordsQuery {
  startTime: string
  endTime: string
  currentPage: number
  pageSize: number
  model?: string
}

export interface ClientOpsError {
  id: string
  requestId: string | null
  kind: string
  route: string
  model: string | null
  requestedModel: string | null
  upstreamModel: string | null
  clientTransport: string | null
  upstreamTransport: string | null
  failureClass: string
  upstreamSendState: string | null
  clientStatusCode: number | null
  upstreamStatusCode: number | null
  latencyMs: number | null
  clientIp: string | null
  userAgent: string | null
  reasoningEffort: string | null
  reasoningPreset: string | null
  subagentKind: string | null
  compact: boolean
  message: string
  recoveredAt: string | null
  createdAt: string
  createdAtDisplay: string
}

export interface ClientOpsErrorsResponse {
  items: ClientOpsError[]
  currentPage: number
  pageSize: number
  total: number
}

export type ClientUsageInsightsResponse = UsageInsightsOverviewResponse

export interface ClientKeySummary {
  name: string
  label: string | null
  prefix: string
  lastUsedAt?: string
}

export interface ClientBudgetWindow {
  limitUsd: string
  usedUsd: string
  remainingUsd: string | null
  startsAt: string | null
  resetsAt: string | null
}

export interface ClientOverviewResponse {
  asOf: string
  timezone: 'Asia/Shanghai'
  currency: 'USD'
  key: ClientKeySummary
  budget: {
    daily: ClientBudgetWindow
    weekly: ClientBudgetWindow
  }
  limits: {
    maxConcurrency: number
    requestsPerMinute: number
  }
  today: {
    range: {
      startsAt: string
      endsAt: string
      granularity: '15m' | 'hour' | 'day'
    }
    totals: {
      requestCount: number
      successCount: number
      failureCount: number
      cancelledCount: number
      incompleteCount: number
      inputTokens: number
      outputTokens: number
      cachedTokens: number
      totalTokens: number
      averageFirstTokenLatencyMs: number | null
    }
    billedUsd: string
    trend: Array<{
      bucketStart: string
      requestCount: number
      successCount: number
      failureCount: number
      inputTokens: number
      outputTokens: number
      cachedTokens: number
      totalTokens: number
      firstTokenP50Ms: number | null
      firstTokenP95Ms: number | null
      latencyP50Ms: number | null
      latencyP95Ms: number | null
      outputThroughputP50: number | null
      billedUsd: string
    }>
  }
  lifetime: {
    requestCount: number
    inputTokens: number
    cachedTokens: number
    totalTokens: number
    billedUsd: string
  }
  healthTimeline: DashboardHealthTimeline
  wireProfiles: DashboardWireProfile[]
  usageRecords: ClientUsageRecord[]
}

export function getClientOverview(options: RequestOptions = {}) {
  return request<ClientOverviewResponse>({
    url: '/api/client/overview',
    method: 'GET',
    ...options,
  })
}

export function getClientSystemVersion(options: RequestOptions = {}) {
  return request<{ version: string }>({
    url: '/api/client/system/version',
    method: 'GET',
    ...options,
  })
}

export function getClientUsageRecords(data: ClientUsageRecordsQuery, options: RequestOptions = {}) {
  return request<ClientUsageRecordsResponse>({
    url: '/api/client/usage/records',
    method: 'GET',
    params: data,
    ...options,
  })
}

type ClientUsageRangeQuery = Pick<ClientUsageRecordsQuery, 'startTime' | 'endTime'>

export function getClientUsageSummary(data: ClientUsageRangeQuery, options: RequestOptions = {}) {
  return request<UsageSummaryResponse>({
    url: '/api/client/usage/records/summary',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getClientUsageInsights(data: ClientUsageRangeQuery, options: RequestOptions = {}) {
  return request<ClientUsageInsightsResponse>({
    url: '/api/client/usage/insights/overview',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getClientUsageDiagnostics(
  data: ClientUsageRangeQuery & { dimension: string },
  options: RequestOptions = {},
) {
  return request<UsageDiagnosticsResponse>({
    url: '/api/client/usage/insights/diagnostics',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getClientOpsErrors(data: ClientUsageRecordsQuery, options: RequestOptions = {}) {
  return request<ClientOpsErrorsResponse>({
    url: '/api/client/operations/errors',
    method: 'GET',
    params: data,
    ...options,
  })
}
