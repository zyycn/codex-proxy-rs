import { z } from 'zod'

import { requestParsed } from '../request'

const trendKindSchema = z.enum(['usage', 'latency', 'errors'])

const trendPointSchema = z
  .object({
    time: z.string(),
    requests: z.string(),
    requestsValue: z.number(),
    inputTokens: z.string(),
    inputTokensValue: z.number(),
    outputTokens: z.string(),
    outputTokensValue: z.number(),
    cachedTokens: z.string(),
    cachedTokensValue: z.number(),
    cacheHitRateValue: z.number(),
    tokensValue: z.number(),
    errors: z.string(),
    errorsValue: z.number(),
    latency: z.string(),
    latencyValue: z.number().nullable(),
    maxLatency: z.string(),
    maxLatencyValue: z.number().nullable(),
    minLatency: z.string(),
    minLatencyValue: z.number().nullable(),
    successRate: z.string(),
    successRateValue: z.number().nullable(),
  })
  .passthrough()

const dashboardTrendSchema = z.object({
  kind: trendKindSchema,
  points: z.array(trendPointSchema),
  summary: z.array(
    z.object({
      label: z.string(),
      value: z.string(),
      ratio: z.string().nullable(),
    }),
  ),
})

const healthPointSchema = z.object({
  time: z.string(),
  status: z.enum(['future', 'no_data', 'unavailable', 'unstable', 'low_sample', 'stable']),
  reliabilityDisplay: z.string(),
  successRequests: z.number(),
  failedRequests: z.number(),
  cancelledRequests: z.number(),
  callerErrorRequests: z.number(),
})

const healthTimelineSchema = z.object({
  title: z.string(),
  description: z.string(),
  reliabilityDisplay: z.string(),
  status: healthPointSchema.shape.status,
  successRequests: z.number(),
  failedRequests: z.number(),
  cancelledRequests: z.number(),
  callerErrorRequests: z.number(),
  points: z.array(healthPointSchema),
})

const tokenDetailsSchema = z
  .object({
    inputTokens: z.number(),
    outputTokens: z.number(),
    cachedTokens: z.number(),
    cacheWriteTokens: z.number(),
    reasoningTokens: z.number(),
    totalTokens: z.number(),
  })
  .passthrough()

const billingSchema = z
  .object({
    totalAmount: z.number(),
    totalAmountDisplay: z.string(),
  })
  .passthrough()

const dashboardUsageRecordSchema = z
  .object({
    id: z.string(),
    route: z.string().nullable(),
    model: z.string(),
    statusCode: z.number(),
    transport: z.string().nullable(),
    createdAtDisplay: z.string(),
    accountEmail: z.string().nullable(),
    requestedModel: z.string().nullable(),
    upstreamModel: z.string().nullable(),
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
    latencyMs: z.number().nullable(),
    latencyDetails: z.object({
      firstReasoningMs: z.number().nullable(),
      firstTextMs: z.number().nullable(),
      transportDecisionWaitMs: z.number().nullable(),
      wsConnectMs: z.number().nullable(),
      upstreamHeadersMs: z.number().nullable(),
      firstEventMs: z.number().nullable(),
      openaiProcessingMs: z.number().nullable(),
    }),
    firstTokenLatencyMsDisplay: z.string(),
    latencyMsDisplay: z.string(),
  })
  .passthrough()

const wireProfileSchema = z
  .object({
    originator: z.string(),
    codexVersion: z.string(),
    desktopVersion: z.string(),
    desktopBuild: z.string(),
    target: z.object({
      osType: z.string(),
      osVersion: z.string(),
      arch: z.string(),
      terminal: z.string(),
    }),
    userAgent: z.string(),
    verifiedAt: z.string(),
    release: z
      .object({
        status: z.string(),
        checkedAt: z.string().optional(),
        latestVersion: z.string().optional(),
        latestBuild: z.string().optional(),
        publishedAt: z.string().optional(),
        minimumSystemVersion: z.string().optional(),
        hardwareRequirements: z.string().optional(),
        downloadUrl: z.string().optional(),
        downloadSize: z.number().optional(),
        signaturePresent: z.boolean().optional(),
        error: z.string().optional(),
      })
      .passthrough(),
  })
  .passthrough()

const dashboardSummarySchema = z.object({
  cards: z.object({
    accounts: z.object({
      total: z.string(),
      totalValue: z.number(),
      enabled: z.string(),
      enabledValue: z.number(),
      abnormal: z.string(),
      abnormalValue: z.number(),
    }),
    traffic: z.object({
      todayRequests: z.string(),
      todayRequestsValue: z.number(),
      yesterdayRequestsValue: z.number(),
      totalRequests: z.string(),
    }),
    tokens: z.object({
      todayTokens: z.string(),
      todayTokensValue: z.number(),
      yesterdayTokensValue: z.number(),
      totalTokens: z.string(),
      totalBillingAmountUsd: z.string(),
    }),
    cache: z.object({
      todayHitRate: z.string(),
      todayHitRateValue: z.number().nullable(),
      yesterdayHitRateValue: z.number().nullable(),
      totalHitRate: z.string(),
      totalCachedTokens: z.string(),
      averageFirstTokenLatencyMs: z.string(),
    }),
  }),
  trend: dashboardTrendSchema,
  healthTimeline: healthTimelineSchema,
  accountUsage: z.array(
    z.object({
      id: z.string(),
      email: z.string(),
      planType: z.string().nullable(),
      tokens: z.string(),
      quotaUsedPercent: z.number().nullable(),
      lastUsed: z.string(),
    }),
  ),
  wireProfile: wireProfileSchema,
  usageRecords: z.array(dashboardUsageRecordSchema),
  poolSummary: z.object({
    total: z.number(),
    active: z.number(),
    expired: z.number(),
    quotaExhausted: z.number(),
    refreshing: z.number(),
    disabled: z.number(),
    banned: z.number(),
  }),
  capacityInfo: z.object({
    maxConcurrentPerAccount: z.number(),
    totalSlots: z.number(),
    usedSlots: z.number(),
    availableSlots: z.number(),
  }),
  rotationStrategy: z.string().nullable(),
})

const _dashboardQuerySchema = z.object({
  kind: trendKindSchema.optional(),
})

export function getDashboardSummary(params?: z.input<typeof _dashboardQuerySchema>) {
  return requestParsed({
    url: '/api/admin/dashboard/summary',
    method: 'GET',
    params,
  }, dashboardSummarySchema)
}

export function getDashboardTrend(params: z.input<typeof _dashboardQuerySchema>) {
  return requestParsed({
    url: '/api/admin/dashboard/trend',
    method: 'GET',
    params,
  }, dashboardTrendSchema)
}
