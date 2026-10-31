import type { RequestOptions } from '../request'
import type { AnalyticsMetadata } from './analytics'
import { API_BASE_URL } from '../constants'
import request from '../request'

export interface UsageTokenDetails {
  inputTokens: number | null
  outputTokens: number | null
  cachedInputTokens: number | null
  cacheWriteInputTokens: number | null
  reasoningOutputTokens: number | null
  imageInputTokens: number | null
  imageOutputTokens: number | null
  totalTokens: number | null
  inputTokensDisplay: string
  outputTokensDisplay: string
  cachedInputTokensDisplay: string
  cacheWriteInputTokensDisplay: string
  reasoningOutputTokensDisplay: string
  imageInputTokensDisplay: string
  imageOutputTokensDisplay: string
  totalTokensDisplay: string
}

export interface UsageCost {
  currency: string
  estimatedAmount: string
}

export interface UsageCostCoverage {
  known: number
  partial: number
  unknown: number
  notBillable: number
}

export interface UsageBilling {
  longContextBillingApplied: boolean
  image?: {
    inputAmountDisplay: string
    cacheReadAmountDisplay: string
    inputPriceDisplay: string
    cacheReadPriceDisplay: string
  }
  inputAmountDisplay: string
  outputAmountDisplay: string
  cacheReadAmountDisplay: string
  cacheWriteAmountDisplay: string
  standardAmountDisplay: string
  totalAmountDisplay: string
  inputPriceDisplay: string
  outputPriceDisplay: string
  cacheReadPriceDisplay: string
  cacheWritePriceDisplay: string
  serviceTierDisplay: string
  multiplierDisplay: string
}

export interface UsageLatencyDetails {
  upstreamResponseMs?: number
  upstreamApiOverheadMs?: number
  upstreamEngineMs?: number
  admissionDecisionMs?: number
  accountSelectionWaitMs?: number
  capacityUsedSlots?: number
  capacityTotalSlots?: number
  transportDecisionWaitMs?: number
  wsConnectMs?: number
  upstreamHeadersMs?: number
  firstEventMs?: number
  firstReasoningMs?: number
  firstTextMs?: number
  openaiProcessingMs?: number
}

// Provider 私有观测字段；Core 字段由顶层 UsageRecord 提供，不再复制进 metadata。
export interface UsageRecordMetadata {
  [key: string]: unknown
}

export type UsageRecordStatus = 'success' | 'error' | 'all'

export interface UsageListRecord {
  logicalOutcome: string
  errorCode: string | null
  errorMessage: string | null
  clientApiKeyName: string | null
  id: string
  provider: string | null
  authenticationKind: string | null
  accountId: string | null
  accountEmail: string | null
  accountName: string | null
  accountNotes: string | null
  accountPlanType: string | null
  accountPlanTypeDisplay: string | null
  route: string
  model: string | null
  requestedModel: string | null
  upstreamModel: string | null
  serviceTier: string | null
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

export interface UsageRecord {
  id: string
  requestId: string
  clientApiKeyId: string | null
  kind: string
  provider: string | null
  authenticationKind: string | null
  accountId: string | null
  accountEmail: string | null
  accountName: string | null
  route: string
  model: string | null
  requestedModel: string | null
  upstreamModel: string | null
  serviceTier: string | null
  statusCode: number | null
  clientTransport: string
  upstreamTransport: string | null
  protocol: string
  httpVersion: string | null
  clientStatusCode: number | null
  upstreamStatusCode: number | null
  websocketPool: { kind: string } | null
  imageGenerationRequested: boolean
  imageGenerationSucceeded: boolean | null
  latencyDetails: UsageLatencyDetails | null
  attemptIndex: number | null
  attemptCount: number
  responseId: string | null
  upstreamRequestId: string | null
  latencyMs: number | null
  inputTokens: number | null
  outputTokens: number | null
  cachedInputTokens: number | null
  cacheWriteInputTokens: number | null
  reasoningOutputTokens: number | null
  imageInputTokens: number | null
  imageOutputTokens: number | null
  message: string
  metadata: UsageRecordMetadata
  createdAt: string
  createdAtDisplay: string
  clientIp: string | null
  userAgent: string | null
  reasoningEffort: string | null
  reasoningPreset: string | null
  compact: boolean | null
  requestKind: string | null
  subagentKind: string | null
  tokenDetails: UsageTokenDetails
  billing: UsageBilling | null
  costs: UsageCost[]
  costCoverage: UsageCostCoverage
  firstTokenLatencyMs: number | null
  latencyMsDisplay: string
  logicalOutcome: string
}

export interface UsageAttempt {
  id: string
  attemptIndex: number
  trigger: string
  provider: string
  model: string | null
  transport: string
  sendState: string
  outcome: string
  downstreamCommitted: boolean
  statusCode: number | null
  providerErrorCode: string | null
  failureClass: string | null
  costEstimateStatus: string
  estimatedCostAmount: string | null
  estimatedCostCurrency: string | null
  inputTokens: number | null
  outputTokens: number | null
  cachedInputTokens: number | null
  totalTokens: number | null
  firstTokenMs: number | null
  latencyMs: number | null
  credentialName: string | null
  accountId: string | null
  accountName: string | null
  accountEmail: string | null
  authenticationKind: string | null
  startedAt: string
  completedAt: string | null
}

export type UsageRecordDetail = UsageRecord & {
  attempts: UsageAttempt[]
  /** 尝试列表是否完整；best-effort 下恒为 false。 */
  attemptsComplete: boolean
}

export interface UsageRecordsResponse {
  items: UsageListRecord[]
  currentPage: number
  pageSize: number
  total: number
}

export interface UsageSummaryResponse {
  analytics: AnalyticsMetadata
  totalRequests: string
  inputTokens: string
  outputTokens: string
  cachedInputTokens: string
  cacheWriteInputTokens: string
  totalTokens: string
  averageLatencyMs: string
  averageLatencyMsValue: number | null
}

export interface UsageOverviewHealthPoint {
  bucket: string
  label: string
  totalRequests: number
  successRequests: number
  failedRequests: number
  cancelledRequests: number
  incompleteRequests: number
  callerErrorRequests: number
  errorRate: number
}

export interface UsageOverviewHealth {
  totalRequests: number
  successRequests: number
  failedRequests: number
  cancelledRequests: number
  incompleteRequests: number
  callerErrorRequests: number
  successRate: number
  completionRate: number
  requestChangeRate: number | null
  successRateChange: number | null
  points: UsageOverviewHealthPoint[]
}

export interface UsageOverviewLoad {
  averageConcurrency: number
  peakConcurrency: number
  admissionDecisionP95Ms: number | null
  accountSelectionWaitP95Ms: number | null
}

export interface UsageOverviewPerformanceMetrics {
  latencyP50Ms: number | null
  latencyP95Ms: number | null
  latencyP99Ms: number | null
  firstTokenP50Ms: number | null
  firstTokenP95Ms: number | null
  firstTokenP99Ms: number | null
  outputThroughputP10: number | null
  outputThroughputP50: number | null
  outputThroughputP90: number | null
  load: UsageOverviewLoad
}

export interface UsageOverviewPerformancePoint extends UsageOverviewPerformanceMetrics {
  bucket: string
  label: string
}

export interface UsageOverviewPerformance extends UsageOverviewPerformanceMetrics {
  points: UsageOverviewPerformancePoint[]
}

export interface UsageOverviewCostPoint {
  bucket: string
  label: string
  inputTokens: number
  outputTokens: number
  cachedInputTokens: number
  totalTokens: number
  estimatedCost: string | null
  standardCost: string | null
  noCacheCost: string | null
  cacheSavings: string | null
  cachedTokenRate: number
  cacheHitRequestRate: number | null
}

export interface UsageOverviewCost {
  estimatedCost: string | null
  standardCost: string | null
  noCacheCost: string | null
  cacheSavings: string | null
  tierPremium: string | null
  costPerRequest: string | null
  costPerSuccessfulRequest: string | null
  tokensPerRequest: number
  cachedTokenRate: number
  cacheHitRequestRate: number | null
  inputTokens: number
  outputTokens: number
  cachedInputTokens: number
  totalTokens: number
  points: UsageOverviewCostPoint[]
  coverage: UsageCostCoverage
}

export interface UsageInsightsOverviewResponse {
  analytics: AnalyticsMetadata
  granularity: string
  health: UsageOverviewHealth
  performance: UsageOverviewPerformance
  cost: UsageOverviewCost
}

export type UsagePerformanceMetric = 'latency' | 'firstToken' | 'throughput' | 'load'
export type UsageCostMetric = 'cost' | 'savings' | 'cache'

export interface UsageMetricSeries<S, P = S> {
  summary: S
  points: { bucket: string, label: string, requestCount: number, value: P }[]
}

interface UsageLatencyPercentiles {
  p50Ms: number | null
  p95Ms: number | null
  p99Ms: number | null
}

type UsageCostSummary = Pick<UsageOverviewCost, 'estimatedCost' | 'cacheSavings' | 'tierPremium' | 'costPerSuccessfulRequest' | 'coverage'>

interface UsageMetricData {
  health: UsageOverviewHealth
  latency: UsageMetricSeries<UsageLatencyPercentiles>
  firstToken: UsageMetricSeries<UsageLatencyPercentiles>
  throughput: UsageMetricSeries<{ p10: number | null, p50: number | null, p90: number | null }>
  load: UsageMetricSeries<UsageOverviewLoad>
  cost: UsageMetricSeries<UsageCostSummary, { estimatedCost: string | null, noCacheCost: string | null }>
  savings: UsageMetricSeries<UsageCostSummary, string | null>
  cache: UsageMetricSeries<UsageCostSummary, { inputTokens: number, cachedTokenRate: number, cacheHitRequestRate: number | null }>
}

export type UsageMetricResponse<M extends keyof UsageMetricData> = {
  [K in M]: { analytics: AnalyticsMetadata, granularity: string, metric: K, data: UsageMetricData[K] }
}[M]

export interface UsageDiagnosticItem {
  key: string
  name: string
  authenticationKind: string | null
  accountPlanType: string | null
  accountPlanTypeDisplay: string | null
  requestCount: number
  successCount: number
  errorCount: number
  errorRate: number
  requestShare: number
  averageLatencyMs: number | null
  latencyP95Ms: number | null
  firstTokenP95Ms: number | null
  nonCompletionCount: number
  nonCompletionRate: number
  retryCount: number
  retryRate: number
  estimatedCost: string | null
  attemptCount: number
  totalTokens: number
}

export interface UsageDiagnosticsResponse {
  analytics: AnalyticsMetadata
  dimension: string
  items: UsageDiagnosticItem[]
}

// 请求参数类型：仅定义 API 边界的形状，调用方不依赖显式声明。
interface UsageRangeQuery {
  startTime: string
  endTime: string
  provider?: string
}

interface PageQuery {
  currentPage: number
  pageSize: number
}

type UsagePageQuery = UsageRangeQuery & PageQuery & {
  clientApiKeyName?: string
  accountName?: string
  model?: string
  status?: UsageRecordStatus
}

interface UsageDetailQuery {
  id: string
}

type UsageDiagnosticsQuery = UsageRangeQuery & { dimension: string, model?: string, statusCode?: number }

type DiagnosticsDownloadParams = { requestId: string } | { errorId: string }

export function getUsageRecords(data: UsagePageQuery, options: RequestOptions = {}) {
  return request<UsageRecordsResponse>({
    url: '/api/admin/usage/records',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getUsageRecordDetail(data: UsageDetailQuery, options: RequestOptions = {}) {
  return request<UsageRecordDetail>({
    url: '/api/admin/usage/records/detail',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getUsageRecordSummary(data: UsageRangeQuery, options: RequestOptions = {}) {
  return request<UsageSummaryResponse>({
    url: '/api/admin/usage/records/summary',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getUsageRecordInsightsOverview(data: UsageRangeQuery, options: RequestOptions = {}) {
  return request<UsageInsightsOverviewResponse>({
    url: '/api/admin/usage/insights/overview',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getUsageRecordInsightsDiagnostics(data: UsageDiagnosticsQuery, options: RequestOptions = {}) {
  return request<UsageDiagnosticsResponse>({
    url: '/api/admin/usage/insights/diagnostics',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getUsageHealth(data: UsageRangeQuery, options: RequestOptions = {}) {
  return request<UsageMetricResponse<'health'>>({
    url: '/api/admin/usage/insights/health',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getUsagePerformance<M extends UsagePerformanceMetric>(data: UsageRangeQuery & { metric: M }, options: RequestOptions = {}) {
  return request<UsageMetricResponse<M>>({
    url: '/api/admin/usage/insights/performance',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getUsageCost<M extends UsageCostMetric>(data: UsageRangeQuery & { metric: M }, options: RequestOptions = {}) {
  return request<UsageMetricResponse<M>>({
    url: '/api/admin/usage/insights/cost',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getDiagnosticsDownloadUrl(data: DiagnosticsDownloadParams) {
  return `${API_BASE_URL}/api/admin/diagnostics/export?${new URLSearchParams(data)}`
}
