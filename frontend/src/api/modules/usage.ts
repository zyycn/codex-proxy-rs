import request from '../request'

export interface UsagePageMeta {
  page: number
  pageSize: number
  total: number
  totalPages: number
}

export interface UsageTokenDetails {
  inputTokens: number | null
  outputTokens: number | null
  cachedTokens: number | null
  cacheWriteTokens: number | null
  reasoningTokens: number | null
  imageInputTokens: number | null
  imageOutputTokens: number | null
  totalTokens: number | null
  inputTokensDisplay: string
  outputTokensDisplay: string
  cachedTokensDisplay: string
  cacheWriteTokensDisplay: string
  reasoningTokensDisplay: string
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
  transportDecisionWaitMs?: number
  wsConnectMs?: number
  upstreamHeadersMs?: number
  firstEventMs?: number
  firstReasoningMs?: number
  firstTextMs?: number
  firstTokenMs?: number
  openaiProcessingMs?: number
}

// 已知字段之外还会平铺 Provider 安全观测字段，因此保留索引签名。
export interface UsageRecordMetadata {
  [key: string]: unknown
  protocol: string
  logicalOutcome: string
  attemptCount: number
  requestedModel: string
  upstreamModel?: string
  clientIp?: string
  userAgent?: string
  reasoningEffort?: string
  reasoningPreset?: string
  compact: boolean
  requestKind?: string
  subagentKind?: string
  transport?: string
  httpVersion?: string
  clientStatusCode?: number
  upstreamStatusCode?: number
  responseId?: string
  upstreamRequestId?: string
  websocketPool: { kind: string } | null
  imageGenerationRequested: boolean
  imageGenerationSucceeded: boolean | null
  latencyDetails: UsageLatencyDetails
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
  model: string
  requestedModel: string | null
  upstreamModel: string | null
  serviceTier: string | null
  statusCode: number | null
  transport: string | null
  attemptIndex: number | null
  attemptCount: number
  responseId: string | null
  upstreamRequestId: string | null
  latencyMs: number | null
  firstTokenMs: number | null
  inputTokens: number | null
  outputTokens: number | null
  cachedTokens: number | null
  cacheWriteTokens: number | null
  reasoningTokens: number | null
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
  firstTokenLatencyMsDisplay: string
  latencyMsDisplay: string
  logicalOutcome: string
}

export interface UsageAttempt {
  id: string
  attemptIndex: number
  trigger: string
  provider: string
  model: string
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
  cachedTokens: number | null
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
  items: UsageRecord[]
  page: UsagePageMeta
  nextCursor: string | null
}

export interface OpsErrorMetadata {
  source: string
  component: string
  attemptId: string | null
  accountLabel: string | null
}

export interface OpsError {
  id: string
  requestId: string | null
  clientApiKeyId: string | null
  kind: string
  provider: string | null
  authenticationKind: string | null
  accountId: string | null
  route: string
  model: string | null
  clientStatusCode: number | null
  upstreamStatusCode: number | null
  transport: string | null
  attemptIndex: number | null
  failureClass: string
  responseId: string | null
  upstreamRequestId: string | null
  latencyMs: number | null
  message: string
  metadata: OpsErrorMetadata
  createdAt: string
  createdAtDisplay: string
}

export interface OpsErrorsResponse {
  items: OpsError[]
  page: UsagePageMeta
  nextCursor: string | null
}

export interface UsageSummaryResponse {
  totalRequests: string
  inputTokens: string
  outputTokens: string
  cachedTokens: string
  cacheWriteTokens: string
  totalTokens: string
  averageLatencyMs: string
}

export interface UsageOverviewHealthPoint {
  bucket: string
  label: string
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
  requestChangeRate: number | null
  successRateChange: number | null
  points: UsageOverviewHealthPoint[]
}

export interface UsageOverviewPerformancePoint {
  bucket: string
  label: string
  latencyP50Ms: number | null
  latencyP95Ms: number | null
  latencyP99Ms: number | null
  firstTokenP50Ms: number | null
  firstTokenP95Ms: number | null
  firstTokenP99Ms: number | null
}

export interface UsageOverviewPerformance {
  latencyP50Ms: number | null
  latencyP95Ms: number | null
  latencyP99Ms: number | null
  firstTokenP50Ms: number | null
  firstTokenP95Ms: number | null
  firstTokenP99Ms: number | null
  latencyCoverage: number
  firstTokenCoverage: number
  points: UsageOverviewPerformancePoint[]
}

export interface UsageOverviewCostPoint {
  bucket: string
  label: string
  inputTokens: number
  outputTokens: number
  cachedTokens: number
  totalTokens: number
  estimatedCost: string | null
  standardCost: string | null
  cachedTokenRate: number
  cacheHitRequestRate: number | null
}

export interface UsageOverviewCost {
  estimatedCost: string | null
  standardCost: string | null
  costPerRequest: string | null
  tokensPerRequest: number
  cachedTokenRate: number
  cacheHitRequestRate: number | null
  inputTokens: number
  outputTokens: number
  cachedTokens: number
  totalTokens: number
  points: UsageOverviewCostPoint[]
}

export interface UsageInsightsOverviewResponse {
  granularity: string
  health: UsageOverviewHealth
  performance: UsageOverviewPerformance
  cost: UsageOverviewCost
}

export interface UsageDiagnosticItem {
  key: string
  name: string
  requestCount: number
  successCount: number
  errorCount: number
  errorRate: number
  requestShare: number
  averageLatencyMs: number | null
  latencyP95Ms: number | null
  estimatedCost: string | null
  attemptCount: number
  totalTokens: number
}

export interface UsageDiagnosticsResponse {
  dimension: string
  items: UsageDiagnosticItem[]
}

// 请求参数类型：仅定义 API 边界的形状，调用方不依赖显式声明。
interface UsageRangeQuery {
  startTime: string
  endTime: string
  provider?: string
  model?: string
  statusCode?: number
  search?: string
}

type UsagePagedQuery = UsageRangeQuery & {
  page: number
  pageSize: number
}

interface UsageDetailQuery {
  id: string
}

type UsageDiagnosticsQuery = UsageRangeQuery & { dimension: string }

export function getUsageRecords(data: UsagePagedQuery) {
  return request<UsageRecordsResponse>({
    url: '/api/admin/usage/records',
    method: 'GET',
    params: data,
  })
}

export function getOpsErrors(data: UsagePagedQuery) {
  return request<OpsErrorsResponse>({
    url: '/api/admin/operations/errors',
    method: 'GET',
    params: data,
  })
}

export function getUsageRecordDetail(data: UsageDetailQuery) {
  return request<UsageRecordDetail>({
    url: '/api/admin/usage/records/detail',
    method: 'GET',
    params: data,
  })
}

export function getUsageRecordSummary(data: UsageRangeQuery) {
  return request<UsageSummaryResponse>({
    url: '/api/admin/usage/records/summary',
    method: 'GET',
    params: data,
  })
}

export function getUsageRecordInsightsOverview(data: UsageRangeQuery) {
  return request<UsageInsightsOverviewResponse>({
    url: '/api/admin/usage/insights/overview',
    method: 'GET',
    params: data,
  })
}

export function getUsageRecordInsightsDiagnostics(data: UsageDiagnosticsQuery) {
  return request<UsageDiagnosticsResponse>({
    url: '/api/admin/usage/insights/diagnostics',
    method: 'GET',
    params: data,
  })
}
