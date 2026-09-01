import request from '../request'

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
  firstTokenMs?: number
  openaiProcessingMs?: number
}

// Provider 私有观测字段；Core 字段由顶层 UsageRecord 提供，不再复制进 metadata。
export interface UsageRecordMetadata {
  [key: string]: unknown
}

export interface UsageListRecord {
  id: string
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
  items: UsageListRecord[]
  currentPage: number
  pageSize: number
  total: number
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
  operation: string
  protocol: string | null
  clientTransport: string | null
  provider: string | null
  authenticationKind: string | null
  accountId: string | null
  accountName: string | null
  accountEmail: string | null
  route: string
  model: string | null
  requestedModel: string | null
  upstreamModel: string | null
  serviceTier: string | null
  clientStatusCode: number | null
  upstreamStatusCode: number | null
  transport: string | null
  attemptIndex: number | null
  failureClass: string
  upstreamSendState: string | null
  providerErrorCode: string | null
  occurrenceCount: number
  responseId: string | null
  upstreamRequestId: string | null
  latencyMs: number | null
  clientIp: string | null
  userAgent: string | null
  reasoningEffort: string | null
  reasoningPreset: string | null
  requestKind: string | null
  subagentKind: string | null
  compact: boolean | null
  message: string
  rawUpstreamError: string | null
  metadata: OpsErrorMetadata
  createdAt: string
  createdAtDisplay: string
}

export interface OpsErrorsResponse {
  items: OpsError[]
  currentPage: number
  pageSize: number
  total: number
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

export interface UsageOverviewPerformancePoint {
  bucket: string
  label: string
  latencyP50Ms: number | null
  latencyP95Ms: number | null
  latencyP99Ms: number | null
  firstTokenP50Ms: number | null
  firstTokenP95Ms: number | null
  firstTokenP99Ms: number | null
  admissionDecisionP50Ms: number | null
  admissionDecisionP95Ms: number | null
  accountSelectionWaitP50Ms: number | null
  accountSelectionWaitP95Ms: number | null
  outputThroughputP10: number | null
  outputThroughputP50: number | null
  outputThroughputP90: number | null
  capacityUtilization: number | null
  capacityUtilizationP95: number | null
}

export interface UsageOverviewPerformance {
  latencyP50Ms: number | null
  latencyP95Ms: number | null
  latencyP99Ms: number | null
  firstTokenP50Ms: number | null
  firstTokenP95Ms: number | null
  firstTokenP99Ms: number | null
  admissionDecisionP50Ms: number | null
  admissionDecisionP95Ms: number | null
  accountSelectionWaitP50Ms: number | null
  accountSelectionWaitP95Ms: number | null
  outputThroughputP10: number | null
  outputThroughputP50: number | null
  outputThroughputP90: number | null
  capacityUtilization: number | null
  capacityUtilizationP95: number | null
  latencyCoverage: number
  firstTokenCoverage: number
  admissionDecisionCoverage: number
  accountSelectionWaitCoverage: number
  capacityCoverage: number
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
  cachedTokens: number
  totalTokens: number
  points: UsageOverviewCostPoint[]
  coverage: UsageCostCoverage
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
  firstTokenP95Ms: number | null
  nonCompletionCount: number
  nonCompletionRate: number
  retryCount: number
  retryRate: number
  impactScore: number
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

interface PageQuery {
  currentPage: number
  pageSize: number
}

type UsagePageQuery = UsageRangeQuery & PageQuery

type OpsErrorPageQuery = UsagePageQuery & {
  failureClass?: string
  route?: string
}

interface UsageDetailQuery {
  id: string
}

type UsageDiagnosticsQuery = UsageRangeQuery & { dimension: string }

export function getUsageRecords(data: UsagePageQuery) {
  return request<UsageRecordsResponse>({
    url: '/api/admin/usage/records',
    method: 'GET',
    params: data,
  })
}

export function getOpsErrors(data: OpsErrorPageQuery) {
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
