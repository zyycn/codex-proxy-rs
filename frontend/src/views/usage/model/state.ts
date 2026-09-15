import type {
  UsageDiagnosticsResponse,
  UsageInsightsOverviewResponse,
  UsageSummaryResponse,
} from '@/api'

export function emptyUsageSummary(): UsageSummaryResponse {
  return {
    totalRequests: '0',
    inputTokens: '0',
    outputTokens: '0',
    cachedTokens: '0',
    cacheWriteTokens: '0',
    totalTokens: '0',
    averageLatencyMs: '0 ms',
  }
}

export function emptyUsageInsights(): {
  overview: UsageInsightsOverviewResponse
  diagnostics: UsageDiagnosticsResponse
} {
  return {
    overview: {
      granularity: '1d',
      health: {
        totalRequests: 0,
        successRequests: 0,
        failedRequests: 0,
        cancelledRequests: 0,
        incompleteRequests: 0,
        callerErrorRequests: 0,
        successRate: 0,
        completionRate: 0,
        requestChangeRate: null,
        successRateChange: null,
        points: [],
      },
      performance: {
        latencyP50Ms: null,
        latencyP95Ms: null,
        latencyP99Ms: null,
        firstTokenP50Ms: null,
        firstTokenP95Ms: null,
        firstTokenP99Ms: null,
        admissionDecisionP50Ms: null,
        admissionDecisionP95Ms: null,
        accountSelectionWaitP50Ms: null,
        accountSelectionWaitP95Ms: null,
        outputThroughputP10: null,
        outputThroughputP50: null,
        outputThroughputP90: null,
        capacityUtilization: null,
        capacityUtilizationP95: null,
        latencyCoverage: 0,
        firstTokenCoverage: 0,
        admissionDecisionCoverage: 0,
        accountSelectionWaitCoverage: 0,
        capacityCoverage: 0,
        points: [],
      },
      cost: {
        estimatedCost: null,
        standardCost: null,
        noCacheCost: null,
        cacheSavings: null,
        tierPremium: null,
        costPerRequest: null,
        costPerSuccessfulRequest: null,
        tokensPerRequest: 0,
        cachedTokenRate: 0,
        cacheHitRequestRate: 0,
        inputTokens: 0,
        outputTokens: 0,
        cachedTokens: 0,
        totalTokens: 0,
        points: [],
        coverage: { known: 0, partial: 0, unknown: 0, notBillable: 0 },
      },
    },
    diagnostics: {
      dimension: 'model',
      items: [],
    },
  }
}
