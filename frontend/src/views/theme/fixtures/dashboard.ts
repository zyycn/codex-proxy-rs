import type {
  DashboardHealthStatus,
  DashboardSummaryResponse,
  DashboardTrendPoint,
} from '@/api/modules/dashboard'
import type { UsageRecord } from '@/api/modules/usage'

interface PreviewUsageRecordOptions {
  id: string
  accountEmail: string
  provider: 'openai' | 'xai'
  authenticationKind: 'oauth' | 'api_key'
  model: string
  clientTransport: 'http_sse' | 'websocket' | 'http_json'
  upstreamTransport: 'http_sse' | 'websocket'
  reasoningEffort: string
  inputTokens: number
  outputTokens: number
  cachedTokens: number
  firstTokenMs: number
  latencyMs: number
  estimatedCost: string
  createdAt: string
  createdAtDisplay: string
  clientIp: string
  userAgent: string
  compact?: boolean
}

const requestSeries = [
  860,
  920,
  780,
  1120,
  1280,
  1050,
  1360,
  1490,
  1320,
  1720,
  1600,
  1880,
  2010,
  1940,
  2240,
  2180,
  2480,
  2350,
  2670,
  2530,
  2860,
  2720,
  3040,
  3180,
]

function previewTrendPoint(requestsValue: number, index: number): DashboardTrendPoint {
  const inputTokensValue = requestsValue * (58 + (index % 4) * 4)
  const outputTokensValue = requestsValue * (18 + (index % 3) * 3)
  const cachedTokensValue = Math.round(inputTokensValue * (0.36 + (index % 5) * 0.035))
  const errorsValue = index % 7 === 0 ? Math.max(2, Math.round(requestsValue * 0.012)) : 0
  const hour = String(index).padStart(2, '0')
  const successRateValue = ((requestsValue - errorsValue) / requestsValue) * 100

  return {
    time: `${hour}:00`,
    bucket: `2026-08-23T${hour}:00:00+08:00`,
    label: `${hour}:00`,
    requests: requestsValue.toLocaleString('zh-CN'),
    requestsValue,
    inputTokens: inputTokensValue.toLocaleString('zh-CN'),
    inputTokensValue,
    outputTokens: outputTokensValue.toLocaleString('zh-CN'),
    outputTokensValue,
    cachedTokens: cachedTokensValue.toLocaleString('zh-CN'),
    cachedTokensValue,
    cacheHitRateValue: cachedTokensValue / inputTokensValue,
    tokensValue: inputTokensValue + outputTokensValue,
    errors: errorsValue.toLocaleString('zh-CN'),
    errorsValue,
    latency: `${410 + index * 7} ms`,
    latencyValue: 410 + index * 7,
    maxLatency: `${760 + index * 12} ms`,
    maxLatencyValue: 760 + index * 12,
    minLatency: `${180 + index * 3} ms`,
    minLatencyValue: 180 + index * 3,
    successRate: `${successRateValue.toFixed(1)}%`,
    successRateValue,
    firstTokenP50Ms: 210 + index * 3,
    firstTokenP95Ms: 420 + index * 5,
    latencyP95Ms: 760 + index * 12,
    outputThroughputP50: 52 + (index % 5),
    admissionDecisionP95Ms: 2 + (index % 3),
    accountSelectionWaitP95Ms: 8 + (index % 4),
    capacityUtilization: 0.42 + (index % 6) * 0.04,
  }
}

function previewHealthStatus(index: number): DashboardHealthStatus {
  if (index === 17)
    return 'unstable'
  if (index === 31)
    return 'low_sample'
  return 'stable'
}

function previewUsageRecord(options: PreviewUsageRecordOptions): UsageRecord {
  const totalTokens = options.inputTokens + options.outputTokens
  const reasoningTokens = Math.round(options.outputTokens * 0.32)
  const stream = options.clientTransport !== 'http_json'

  return {
    id: options.id,
    requestId: `req_${options.id}`,
    clientApiKeyId: 'preview-client-key',
    kind: 'request.completed',
    provider: options.provider,
    authenticationKind: options.authenticationKind,
    accountId: `account_${options.id}`,
    accountEmail: options.accountEmail,
    accountName: options.accountEmail.split('@')[0] ?? null,
    route: stream ? '/v1/responses' : '/v1/chat/completions',
    model: options.model,
    requestedModel: options.model,
    upstreamModel: options.model,
    serviceTier: options.provider === 'openai' ? 'priority' : 'standard',
    statusCode: 200,
    clientTransport: options.clientTransport,
    upstreamTransport: options.upstreamTransport,
    protocol: stream ? 'responses' : 'chat_completions',
    httpVersion: options.upstreamTransport === 'websocket' ? null : 'HTTP/2',
    clientStatusCode: options.clientTransport === 'websocket' ? null : 200,
    upstreamStatusCode: options.upstreamTransport === 'websocket' ? null : 200,
    websocketPool: options.upstreamTransport === 'websocket' ? { kind: 'warm' } : null,
    imageGenerationRequested: false,
    imageGenerationSucceeded: null,
    latencyDetails: {
      transportDecisionWaitMs: 4,
      upstreamHeadersMs: Math.max(24, options.firstTokenMs - 36),
      firstEventMs: options.firstTokenMs,
      firstReasoningMs: options.firstTokenMs + 18,
      firstTextMs: options.firstTokenMs + 64,
      firstTokenMs: options.firstTokenMs,
      openaiProcessingMs: options.provider === 'openai' ? options.latencyMs - 72 : undefined,
    },
    attemptIndex: 0,
    attemptCount: 1,
    responseId: `resp_${options.id}`,
    upstreamRequestId: `up_${options.id}`,
    latencyMs: options.latencyMs,
    firstTokenMs: options.firstTokenMs,
    inputTokens: options.inputTokens,
    outputTokens: options.outputTokens,
    cachedTokens: options.cachedTokens,
    cacheWriteTokens: 0,
    reasoningTokens,
    imageInputTokens: 0,
    imageOutputTokens: 0,
    message: '请求成功',
    metadata: {
      stream,
      apiKind: stream ? 'responses' : 'chat',
    },
    createdAt: options.createdAt,
    createdAtDisplay: options.createdAtDisplay,
    clientIp: options.clientIp,
    userAgent: options.userAgent,
    reasoningEffort: options.reasoningEffort,
    reasoningPreset: options.reasoningEffort,
    compact: options.compact ?? false,
    requestKind: stream ? 'responses' : 'chat',
    subagentKind: null,
    tokenDetails: {
      inputTokens: options.inputTokens,
      outputTokens: options.outputTokens,
      cachedTokens: options.cachedTokens,
      cacheWriteTokens: 0,
      reasoningTokens,
      imageInputTokens: 0,
      imageOutputTokens: 0,
      totalTokens,
      inputTokensDisplay: options.inputTokens.toLocaleString('zh-CN'),
      outputTokensDisplay: options.outputTokens.toLocaleString('zh-CN'),
      cachedTokensDisplay: options.cachedTokens.toLocaleString('zh-CN'),
      cacheWriteTokensDisplay: '0',
      reasoningTokensDisplay: reasoningTokens.toLocaleString('zh-CN'),
      imageInputTokensDisplay: '0',
      imageOutputTokensDisplay: '0',
      totalTokensDisplay: totalTokens.toLocaleString('zh-CN'),
    },
    billing: {
      inputAmountDisplay: options.estimatedCost,
      outputAmountDisplay: options.estimatedCost,
      cacheReadAmountDisplay: '$0.0004',
      cacheWriteAmountDisplay: '$0.0000',
      standardAmountDisplay: options.estimatedCost,
      totalAmountDisplay: options.estimatedCost,
      inputPriceDisplay: '$1.25 / 1M',
      outputPriceDisplay: '$10.00 / 1M',
      cacheReadPriceDisplay: '$0.125 / 1M',
      cacheWritePriceDisplay: '$0.00 / 1M',
      serviceTierDisplay: options.provider === 'openai' ? 'Fast' : 'Standard',
      multiplierDisplay: '1.0x',
    },
    costs: [{ currency: 'USD', estimatedAmount: options.estimatedCost }],
    costCoverage: { known: 1, partial: 0, unknown: 0, notBillable: 0 },
    firstTokenLatencyMs: options.firstTokenMs,
    firstTokenLatencyMsDisplay: `${options.firstTokenMs} ms`,
    latencyMsDisplay: `${options.latencyMs} ms`,
    logicalOutcome: 'success',
  }
}

export const themeDashboardSummary: DashboardSummaryResponse = {
  cards: {
    credentials: {
      total: '52',
      totalValue: 52,
      available: '47',
      availableValue: 47,
      unavailable: '5',
      unavailableValue: 5,
    },
    traffic: {
      todayRequests: '46.9K',
      todayRequestsValue: 46_930,
      yesterdayRequestsValue: 39_640,
      totalRequests: '3.82M',
    },
    tokens: {
      todayTokens: '3.21M',
      todayTokensValue: 3_214_800,
      yesterdayTokensValue: 2_884_100,
      totalTokens: '253M',
      totalBillingAmountUsd: '$195.15',
    },
    cache: {
      todayHitRate: '43.8%',
      todayHitRateValue: 0.438,
      yesterdayHitRateValue: 0.397,
      totalHitRate: '41.2%',
      totalCachedTokens: '86.4M',
      averageFirstTokenLatencyMs: '486 ms',
    },
  },
  trend: {
    kind: 'usage',
    points: requestSeries.map(previewTrendPoint),
    summary: [],
  },
  healthTimeline: {
    title: '请求健康时间线',
    description: '有效请求可用性',
    reliabilityDisplay: '99.2%',
    status: 'stable',
    successRequests: 46_518,
    failedRequests: 412,
    cancelledRequests: 63,
    incompleteRequests: 28,
    callerErrorRequests: 186,
    points: Array.from({ length: 48 }, (_, index) => {
      const hour = String(Math.floor(index / 2)).padStart(2, '0')
      const minute = index % 2 === 0 ? '00' : '30'
      const status = previewHealthStatus(index)
      const failedRequests = status === 'unstable' ? 18 : status === 'low_sample' ? 2 : index % 11

      return {
        time: `${hour}:${minute}`,
        status,
        reliabilityDisplay: status === 'unstable' ? '94.8%' : status === 'low_sample' ? '98.2%' : '99.8%',
        successRequests: 820 + index * 9,
        failedRequests,
        cancelledRequests: index % 9 === 0 ? 2 : 0,
        incompleteRequests: index % 13 === 0 ? 1 : 0,
        callerErrorRequests: index % 6,
      }
    }),
  },
  wireProfiles: [
    {
      provider: 'openai',
      product: 'Codex CLI',
      version: '0.34.0',
      build: '20260820',
      target: {
        osType: 'Mac OS',
        osVersion: '15.6',
        arch: 'arm64',
        terminal: 'iTerm.app',
      },
      userAgent: 'codex_cli_rs/0.34.0',
      attributes: [
        { label: '客户端标识', value: 'codex_cli_rs' },
        { label: 'Token 认证', value: 'bearer' },
      ],
      verifiedAt: '2026-08-23T10:28:00+08:00',
      release: {
        status: 'aligned',
        checkedAt: '2026-08-23T10:30:00+08:00',
        latestVersion: '0.34.0',
        latestBuild: '20260820',
      },
    },
    {
      provider: 'xai',
      product: 'Grok CLI',
      version: '0.7.3',
      target: {
        osType: 'Linux',
        osVersion: '6.8',
        arch: 'x86_64',
        terminal: 'xterm-256color',
      },
      userAgent: 'grok_cli/0.7.3',
      attributes: [
        { label: '客户端标识', value: 'grok_cli' },
        { label: 'Token 认证', value: 'bearer' },
      ],
      verifiedAt: '2026-08-23T10:25:00+08:00',
      release: {
        status: 'review_required',
        checkedAt: '2026-08-23T10:30:00+08:00',
        latestVersion: '0.7.4',
      },
    },
  ],
  accountUsage: [
    {
      id: 'preview-openai-1',
      provider: 'openai',
      authenticationKind: 'oauth',
      email: 'relay@example.com',
      planType: 'team',
      tokens: '926K',
      requestCount: 1_284,
      requestBuckets: [],
      quotaUsedPercent: 32,
      lastUsed: '刚刚',
    },
    {
      id: 'preview-xai-1',
      provider: 'xai',
      authenticationKind: 'oauth',
      email: 'gateway@example.com',
      planType: 'free',
      tokens: '684K',
      requestCount: 894,
      requestBuckets: requestSeries.slice(-12).map((requestCount, index) => ({
        bucketStart: `2026-08-23T${String(index + 12).padStart(2, '0')}:00:00+08:00`,
        requestCount: Math.round(requestCount / 3),
      })),
      quotaUsedPercent: null,
      lastUsed: '2 分钟前',
    },
    {
      id: 'preview-openai-2',
      provider: 'openai',
      authenticationKind: 'oauth',
      email: 'control@example.com',
      planType: 'pro',
      tokens: '512K',
      requestCount: 742,
      requestBuckets: [],
      quotaUsedPercent: 68,
      lastUsed: '5 分钟前',
    },
  ],
  usageRecords: [
    previewUsageRecord({ id: 'preview-01', accountEmail: 'relay@example.com', provider: 'openai', authenticationKind: 'oauth', model: 'gpt-5.6-codex', clientTransport: 'http_sse', upstreamTransport: 'websocket', reasoningEffort: 'high', inputTokens: 18_420, outputTokens: 3_860, cachedTokens: 8_192, firstTokenMs: 486, latencyMs: 3_280, estimatedCost: '$0.0618', createdAt: '2026-08-23T14:42:16+08:00', createdAtDisplay: '14:42:16', clientIp: '10.24.8.16', userAgent: 'codex-cli/0.34.0' }),
    previewUsageRecord({ id: 'preview-02', accountEmail: 'gateway@example.com', provider: 'xai', authenticationKind: 'oauth', model: 'grok-code-fast-1', clientTransport: 'websocket', upstreamTransport: 'http_sse', reasoningEffort: 'medium', inputTokens: 12_760, outputTokens: 2_140, cachedTokens: 4_096, firstTokenMs: 318, latencyMs: 2_460, estimatedCost: '$0.0286', createdAt: '2026-08-23T14:39:52+08:00', createdAtDisplay: '14:39:52', clientIp: '10.24.8.21', userAgent: 'grok-cli/0.7.3' }),
    previewUsageRecord({ id: 'preview-03', accountEmail: 'control@example.com', provider: 'openai', authenticationKind: 'oauth', model: 'gpt-5.5', clientTransport: 'http_sse', upstreamTransport: 'websocket', reasoningEffort: 'xhigh', inputTokens: 32_840, outputTokens: 6_720, cachedTokens: 16_384, firstTokenMs: 572, latencyMs: 5_840, estimatedCost: '$0.1042', createdAt: '2026-08-23T14:35:08+08:00', createdAtDisplay: '14:35:08', clientIp: '10.24.9.11', userAgent: 'codex-desktop/26.818.41705', compact: true }),
    previewUsageRecord({ id: 'preview-04', accountEmail: 'batch@example.com', provider: 'openai', authenticationKind: 'api_key', model: 'gpt-5.2', clientTransport: 'http_json', upstreamTransport: 'http_sse', reasoningEffort: 'low', inputTokens: 4_860, outputTokens: 920, cachedTokens: 0, firstTokenMs: 264, latencyMs: 1_180, estimatedCost: '$0.0124', createdAt: '2026-08-23T14:31:44+08:00', createdAtDisplay: '14:31:44', clientIp: '10.24.12.9', userAgent: 'openai-node/6.4.0' }),
    previewUsageRecord({ id: 'preview-05', accountEmail: 'analysis@example.com', provider: 'xai', authenticationKind: 'api_key', model: 'grok-4.20', clientTransport: 'http_sse', upstreamTransport: 'http_sse', reasoningEffort: 'high', inputTokens: 21_640, outputTokens: 4_380, cachedTokens: 7_168, firstTokenMs: 426, latencyMs: 4_120, estimatedCost: '$0.0731', createdAt: '2026-08-23T14:28:19+08:00', createdAtDisplay: '14:28:19', clientIp: '10.24.15.32', userAgent: 'curl/8.14.1' }),
    previewUsageRecord({ id: 'preview-06', accountEmail: 'scheduler@example.com', provider: 'openai', authenticationKind: 'oauth', model: 'gpt-5.6-codex', clientTransport: 'websocket', upstreamTransport: 'websocket', reasoningEffort: 'medium', inputTokens: 9_760, outputTokens: 1_840, cachedTokens: 3_072, firstTokenMs: 296, latencyMs: 2_020, estimatedCost: '$0.0248', createdAt: '2026-08-23T14:24:03+08:00', createdAtDisplay: '14:24:03', clientIp: '10.24.4.18', userAgent: 'codex-cli/0.34.0' }),
    previewUsageRecord({ id: 'preview-07', accountEmail: 'review@example.com', provider: 'openai', authenticationKind: 'oauth', model: 'gpt-5.5', clientTransport: 'http_sse', upstreamTransport: 'websocket', reasoningEffort: 'high', inputTokens: 15_280, outputTokens: 3_120, cachedTokens: 6_144, firstTokenMs: 448, latencyMs: 3_740, estimatedCost: '$0.0527', createdAt: '2026-08-23T14:18:37+08:00', createdAtDisplay: '14:18:37', clientIp: '10.24.18.7', userAgent: 'codex-desktop/26.818.41705' }),
    previewUsageRecord({ id: 'preview-08', accountEmail: 'fallback@example.com', provider: 'xai', authenticationKind: 'oauth', model: 'grok-code-fast-1', clientTransport: 'websocket', upstreamTransport: 'http_sse', reasoningEffort: 'medium', inputTokens: 7_940, outputTokens: 1_460, cachedTokens: 2_048, firstTokenMs: 342, latencyMs: 1_920, estimatedCost: '$0.0194', createdAt: '2026-08-23T14:13:25+08:00', createdAtDisplay: '14:13:25', clientIp: '10.24.6.23', userAgent: 'grok-cli/0.7.3' }),
    previewUsageRecord({ id: 'preview-09', accountEmail: 'desktop@example.com', provider: 'openai', authenticationKind: 'oauth', model: 'gpt-5.6-codex', clientTransport: 'http_sse', upstreamTransport: 'websocket', reasoningEffort: 'xhigh', inputTokens: 28_420, outputTokens: 5_940, cachedTokens: 12_288, firstTokenMs: 618, latencyMs: 5_260, estimatedCost: '$0.0925', createdAt: '2026-08-23T14:07:11+08:00', createdAtDisplay: '14:07:11', clientIp: '10.24.7.40', userAgent: 'codex-desktop/26.818.41705', compact: true }),
    previewUsageRecord({ id: 'preview-10', accountEmail: 'api@example.com', provider: 'openai', authenticationKind: 'api_key', model: 'gpt-5.2', clientTransport: 'http_json', upstreamTransport: 'http_sse', reasoningEffort: 'low', inputTokens: 3_620, outputTokens: 680, cachedTokens: 0, firstTokenMs: 238, latencyMs: 980, estimatedCost: '$0.0091', createdAt: '2026-08-23T13:58:46+08:00', createdAtDisplay: '13:58:46', clientIp: '10.24.3.14', userAgent: 'openai-python/1.99.0' }),
  ],
  poolSummary: {
    total: 52,
    normal: 47,
    quotaExhausted: 3,
    rateLimited: 1,
    disabled: 0,
    error: 1,
  },
  capacityInfo: {
    maxConcurrentPerAccount: 3,
    totalSlots: 156,
    usedSlots: 84,
    availableSlots: 72,
  },
  rotationStrategy: 'smart',
}
