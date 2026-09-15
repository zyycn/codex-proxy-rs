import type { ClientOverviewResponse, ClientUsageRecord } from '@/api/modules/client'
import { themeDashboardSummary } from './dashboard'

// 只复用固定演示素材，不读取当前 Key 的真实用量或凭据。
const trend: ClientOverviewResponse['today']['trend'] = themeDashboardSummary.trend.points.map((point, index) => {
  const requestCount = Math.round(point.requestsValue / 100)
  const failureCount = index === 7 || index === 17 ? 1 : 0
  const inputTokens = requestCount * 1_800
  const outputTokens = requestCount * 320

  return {
    bucketStart: point.bucket,
    requestCount,
    successCount: requestCount - failureCount,
    failureCount,
    inputTokens,
    outputTokens,
    cachedTokens: Math.round(inputTokens * 0.42),
    totalTokens: inputTokens + outputTokens,
    firstTokenP50Ms: point.firstTokenP50Ms,
    firstTokenP95Ms: point.firstTokenP95Ms,
    latencyP50Ms: point.latencyValue,
    latencyP95Ms: point.latencyP95Ms,
    outputThroughputP50: point.outputThroughputP50,
    billedUsd: index < 16 ? '0.03' : '0.02',
  }
})

const totals = trend.reduce((total, point) => ({
  requestCount: total.requestCount + point.requestCount,
  successCount: total.successCount + point.successCount,
  failureCount: total.failureCount + point.failureCount,
  cancelledCount: 0,
  incompleteCount: 0,
  inputTokens: total.inputTokens + point.inputTokens,
  outputTokens: total.outputTokens + point.outputTokens,
  cachedTokens: total.cachedTokens + point.cachedTokens,
  totalTokens: total.totalTokens + point.totalTokens,
  averageFirstTokenLatencyMs: 486,
}), {
  requestCount: 0,
  successCount: 0,
  failureCount: 0,
  cancelledCount: 0,
  incompleteCount: 0,
  inputTokens: 0,
  outputTokens: 0,
  cachedTokens: 0,
  totalTokens: 0,
  averageFirstTokenLatencyMs: 486,
})

export const themeKeyOverview: ClientOverviewResponse = {
  asOf: '2026-08-23T23:59:00+08:00',
  timezone: 'Asia/Shanghai',
  currency: 'USD',
  key: { name: '主题预览', label: null, prefix: 'preview' },
  budget: {
    daily: {
      limitUsd: '1',
      usedUsd: '0.64',
      remainingUsd: '0.36',
      startsAt: '2026-08-23T00:00:00+08:00',
      resetsAt: '2026-08-24T00:00:00+08:00',
    },
    weekly: {
      limitUsd: '5',
      usedUsd: '2.35',
      remainingUsd: '2.65',
      startsAt: '2026-08-20T10:00:00+08:00',
      resetsAt: '2026-08-27T10:00:00+08:00',
    },
  },
  limits: { maxConcurrency: 0, requestsPerMinute: 0 },
  today: {
    range: {
      startsAt: '2026-08-23T00:00:00+08:00',
      endsAt: '2026-08-24T00:00:00+08:00',
      granularity: 'hour',
    },
    totals,
    billedUsd: '0.64',
    trend,
  },
  lifetime: {
    requestCount: totals.requestCount * 4,
    inputTokens: totals.inputTokens * 4,
    cachedTokens: totals.cachedTokens * 4,
    totalTokens: totals.totalTokens * 4,
    billedUsd: '2.35',
  },
  healthTimeline: {
    title: '请求健康时间线',
    description: '有效请求可用性',
    reliabilityDisplay: `${(totals.successCount / totals.requestCount * 100).toFixed(1)}%`,
    status: 'stable',
    successRequests: totals.successCount,
    failedRequests: totals.failureCount,
    cancelledRequests: 0,
    incompleteRequests: 0,
    callerErrorRequests: 0,
    points: trend.flatMap((point, hour) => Array.from({ length: 4 }, (_, quarter) => {
      const successRequests = Math.floor(point.successCount / 4) + (quarter < point.successCount % 4 ? 1 : 0)
      const failedRequests = quarter === 0 ? point.failureCount : 0
      return {
        time: `${String(hour).padStart(2, '0')}:${String(quarter * 15).padStart(2, '0')}`,
        status: 'low_sample',
        reliabilityDisplay: `${(successRequests / (successRequests + failedRequests) * 100).toFixed(1)}%`,
        successRequests,
        failedRequests,
        cancelledRequests: 0,
        incompleteRequests: 0,
        callerErrorRequests: 0,
      }
    })),
  },
  wireProfiles: [],
  usageRecords: themeDashboardSummary.usageRecords.map((record): ClientUsageRecord => ({
    id: record.id,
    route: record.route,
    model: record.model,
    requestedModel: record.requestedModel,
    upstreamModel: record.upstreamModel,
    clientTransport: record.clientTransport,
    upstreamTransport: record.upstreamTransport,
    reasoningEffort: record.reasoningEffort,
    reasoningPreset: record.reasoningPreset,
    subagentKind: record.subagentKind,
    compact: record.compact,
    tokenDetails: record.tokenDetails,
    billing: record.billing,
    latencyDetails: record.latencyDetails,
    firstTokenLatencyMs: record.firstTokenLatencyMs,
    latencyMs: record.latencyMs,
    createdAt: record.createdAt,
    createdAtDisplay: record.createdAtDisplay,
    clientIp: record.clientIp,
    userAgent: record.userAgent,
  })),
}
