import { z } from 'zod'

import { requestParsed } from '../request'

const pageSchema = z.object({
  page: z.number(),
  pageSize: z.number(),
  total: z.number(),
  totalPages: z.number(),
})

const usageQuerySchema = z.object({
  page: z.number().optional(),
  pageSize: z.number().optional(),
  kind: z.string().optional(),
  clientApiKeyId: z.string().optional(),
  provider: z.string().optional(),
  requestId: z.string().optional(),
  accountId: z.string().optional(),
  route: z.string().optional(),
  model: z.string().optional(),
  statusCode: z.number().optional(),
  transport: z.string().optional(),
  attemptIndex: z.number().optional(),
  responseId: z.string().optional(),
  upstreamRequestId: z.string().optional(),
  search: z.string().optional(),
  startTime: z.string().optional(),
  endTime: z.string().optional(),
})

const tokenDetailsSchema = z.object({
  inputTokens: z.number(),
  outputTokens: z.number(),
  cachedTokens: z.number(),
  cacheWriteTokens: z.number(),
  reasoningTokens: z.number(),
  totalTokens: z.number(),
  inputTokensDisplay: z.string(),
  outputTokensDisplay: z.string(),
  cachedTokensDisplay: z.string(),
  cacheWriteTokensDisplay: z.string(),
  reasoningTokensDisplay: z.string(),
  totalTokensDisplay: z.string(),
})

const billingSchema = z.object({
  inputAmount: z.number(),
  outputAmount: z.number(),
  cacheReadAmount: z.number(),
  cacheWriteAmount: z.number(),
  standardAmount: z.number(),
  totalAmount: z.number(),
  inputPricePerMtoken: z.number(),
  outputPricePerMtoken: z.number(),
  cacheReadPricePerMtoken: z.number(),
  cacheWritePricePerMtoken: z.number(),
  serviceTier: z.string().nullable(),
  serviceTierDisplay: z.string(),
  multiplier: z.number(),
  inputAmountDisplay: z.string(),
  outputAmountDisplay: z.string(),
  cacheReadAmountDisplay: z.string(),
  cacheWriteAmountDisplay: z.string(),
  standardAmountDisplay: z.string(),
  totalAmountDisplay: z.string(),
  inputPriceDisplay: z.string(),
  outputPriceDisplay: z.string(),
  cacheReadPriceDisplay: z.string(),
  cacheWritePriceDisplay: z.string(),
  multiplierDisplay: z.string(),
})

const usageRecordSchema = z
  .object({
    id: z.string(),
    requestId: z.string().nullable(),
    clientApiKeyId: z.string().nullable(),
    kind: z.string(),
    provider: z.string(),
    accountId: z.string(),
    route: z.string().nullable(),
    model: z.string(),
    requestedModel: z.string().nullable(),
    upstreamModel: z.string().nullable(),
    serviceTier: z.string().nullable(),
    statusCode: z.number(),
    transport: z.string().nullable(),
    attemptIndex: z.number().nullable(),
    responseId: z.string().nullable(),
    upstreamRequestId: z.string().nullable(),
    latencyMs: z.number().nullable(),
    firstTokenMs: z.number().nullable(),
    inputTokens: z.number().nullable(),
    outputTokens: z.number().nullable(),
    cachedTokens: z.number().nullable(),
    cacheWriteTokens: z.number().nullable(),
    reasoningTokens: z.number().nullable(),
    message: z.string(),
    metadata: z.unknown(),
    createdAt: z.string(),
    createdAtDisplay: z.string(),
    accountEmail: z.string().nullable(),
    clientIp: z.string().nullable(),
    userAgent: z.string().nullable(),
    reasoningEffort: z.string().nullable(),
    reasoningPreset: z.string().nullable(),
    compact: z.boolean(),
    requestKind: z.string().nullable(),
    subagentKind: z.string().nullable(),
    tokenDetails: tokenDetailsSchema,
    billing: billingSchema.nullable(),
    firstTokenLatencyMs: z.number().nullable(),
    firstTokenLatencyMsDisplay: z.string(),
    latencyMsDisplay: z.string(),
  })
  .passthrough()

const usagePageSchema = z.object({
  items: z.array(usageRecordSchema),
  page: pageSchema,
})

const usageSummarySchema = z.object({
  totalRequests: z.string(),
  inputTokens: z.string(),
  outputTokens: z.string(),
  cachedTokens: z.string(),
  cacheWriteTokens: z.string(),
  totalTokens: z.string(),
  averageLatencyMs: z.string(),
})

const healthPointSchema = z.object({
  bucket: z.string(),
  label: z.string(),
  successRequests: z.number(),
  failedRequests: z.number(),
  cancelledRequests: z.number(),
  callerErrorRequests: z.number(),
  errorRate: z.number(),
})

const performancePointSchema = z.object({
  bucket: z.string(),
  label: z.string(),
  latencyP50Ms: z.number().nullable(),
  latencyP95Ms: z.number().nullable(),
  latencyP99Ms: z.number().nullable(),
  firstTokenP50Ms: z.number().nullable(),
  firstTokenP95Ms: z.number().nullable(),
  firstTokenP99Ms: z.number().nullable(),
})

const costPointSchema = z.object({
  bucket: z.string(),
  label: z.string(),
  inputTokens: z.number(),
  outputTokens: z.number(),
  cachedTokens: z.number(),
  totalTokens: z.number(),
  estimatedCost: z.number(),
  standardCost: z.number(),
  cachedTokenRate: z.number(),
  cacheHitRequestRate: z.number(),
})

const usageOverviewSchema = z.object({
  granularity: z.string(),
  health: z.object({
    totalRequests: z.number(),
    successRequests: z.number(),
    failedRequests: z.number(),
    cancelledRequests: z.number(),
    callerErrorRequests: z.number(),
    successRate: z.number(),
    requestChangeRate: z.number().nullable(),
    successRateChange: z.number().nullable(),
    points: z.array(healthPointSchema),
  }),
  performance: z.object({
    latencyP50Ms: z.number().nullable(),
    latencyP95Ms: z.number().nullable(),
    latencyP99Ms: z.number().nullable(),
    firstTokenP50Ms: z.number().nullable(),
    firstTokenP95Ms: z.number().nullable(),
    firstTokenP99Ms: z.number().nullable(),
    latencyCoverage: z.number(),
    firstTokenCoverage: z.number(),
    points: z.array(performancePointSchema),
  }),
  cost: z.object({
    estimatedCost: z.number(),
    standardCost: z.number(),
    costPerRequest: z.number(),
    tokensPerRequest: z.number(),
    cachedTokenRate: z.number(),
    cacheHitRequestRate: z.number(),
    inputTokens: z.number(),
    outputTokens: z.number(),
    cachedTokens: z.number(),
    totalTokens: z.number(),
    points: z.array(costPointSchema),
  }),
})

const diagnosticsSchema = z.object({
  dimension: z.string(),
  items: z.array(
    z.object({
      name: z.string(),
      requestCount: z.number(),
      successCount: z.number(),
      errorCount: z.number(),
      errorRate: z.number(),
      requestShare: z.number(),
      latencyP95Ms: z.number().nullable(),
      estimatedCost: z.number(),
    }),
  ),
})

const opsErrorSchema = z
  .object({
    id: z.string(),
    requestId: z.string().nullable(),
    clientApiKeyId: z.string().nullable(),
    kind: z.string(),
    provider: z.string().nullable(),
    accountId: z.string().nullable(),
    route: z.string().nullable(),
    model: z.string().nullable(),
    statusCode: z.number().nullable(),
    clientStatusCode: z.number().nullable(),
    upstreamStatusCode: z.number().nullable(),
    transport: z.string().nullable(),
    attemptIndex: z.number().nullable(),
    failureClass: z.string().nullable(),
    responseId: z.string().nullable(),
    upstreamRequestId: z.string().nullable(),
    latencyMs: z.number().nullable(),
    message: z.string(),
    metadata: z.unknown(),
    createdAt: z.string(),
    createdAtDisplay: z.string(),
  })
  .passthrough()

const _opsQuerySchema = usageQuerySchema.extend({
  clientStatusCode: z.number().optional(),
  upstreamStatusCode: z.number().optional(),
  failureClass: z.string().optional(),
})

const _rangeSchema = z.object({
  startTime: z.string().optional(),
  endTime: z.string().optional(),
})

export function getUsageRecords(params?: z.input<typeof usageQuerySchema>) {
  return requestParsed({
    url: '/api/admin/usage/records',
    method: 'GET',
    params,
  }, usagePageSchema)
}

export function getOpsErrors(params?: z.input<typeof _opsQuerySchema>) {
  return requestParsed({
    url: '/api/admin/ops/errors',
    method: 'GET',
    params,
  }, z.object({ items: z.array(opsErrorSchema), page: pageSchema }))
}

export function getUsageRecordDetail(params: { id: string }) {
  return requestParsed({
    url: '/api/admin/usage/records/detail',
    method: 'GET',
    params,
  }, usageRecordSchema)
}

export function getUsageRecordSummary(params?: z.input<typeof _rangeSchema>) {
  return requestParsed({
    url: '/api/admin/usage/records/summary',
    method: 'GET',
    params,
  }, usageSummarySchema)
}

export function getUsageRecordInsightsOverview(params?: z.input<typeof _rangeSchema>) {
  return requestParsed({
    url: '/api/admin/usage/records/insights/overview',
    method: 'GET',
    params,
  }, usageOverviewSchema)
}

export function getUsageRecordInsightsDiagnostics(
  params: z.input<typeof _rangeSchema> & { dimension: string },
) {
  return requestParsed({
    url: '/api/admin/usage/records/insights/diagnostics',
    method: 'GET',
    params,
  }, diagnosticsSchema)
}
