// Usage 列表与详情使用独立读模型；共享展示函数只依赖两者的公共字段。

import type {
  UsageAttempt,
  UsageBilling,
  UsageCost,
  UsageCostCoverage,
  UsageLatencyDetails,
  UsageListRecord,
  UsageRecordDetail,
  UsageTokenDetails,
} from '@/api'

import { isRecord } from '@/utils/object'
import { formatDuration } from './format'

// Usage 记录的规范化 view model：组件只消费这个形状。
export interface UsageViewModel {
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
  attemptIndex: number | null
  attemptCount: number
  responseId: string | null
  upstreamRequestId: string | null
  protocol: string
  httpVersion: string | null
  clientStatusCode: number | null
  upstreamStatusCode: number | null
  websocketPool: { kind: string } | null
  imageGenerationRequested: boolean
  imageGenerationSucceeded: boolean | null
  latencyMs: number | null
  firstTokenLatencyMs: number | null
  latencyDetails: UsageLatencyDetails | null
  inputTokens: number | null
  outputTokens: number | null
  cachedTokens: number | null
  cacheWriteTokens: number | null
  reasoningTokens: number | null
  imageInputTokens: number | null
  imageOutputTokens: number | null
  message: string
  createdAt: string
  createdAtDisplay: string
  clientIp: string | null
  userAgent: string | null
  reasoningEffort: string | null
  reasoningPreset: string | null
  compact: boolean
  requestKind: string | null
  subagentKind: string | null
  tokenDetails: UsageTokenDetails
  billing: UsageBilling | null
  costs: UsageCost[]
  costCoverage: UsageCostCoverage
  firstTokenLatencyMsDisplay: string
  latencyMsDisplay: string
  logicalOutcome: string
  providerMetadata: Record<string, unknown>
  requestBody?: unknown
  responseBody?: unknown
  attempts?: UsageAttempt[]
  attemptsComplete?: boolean
}

/** 将 Usage 详情 API 记录收口为详情组件消费的形状。 */
export function normalizeUsageRecord(record: UsageRecordDetail): UsageViewModel {
  const metadata = record.metadata

  return {
    id: record.id,
    requestId: record.requestId,
    clientApiKeyId: record.clientApiKeyId,
    kind: record.kind,
    provider: record.provider,
    authenticationKind: record.authenticationKind,
    accountId: record.accountId,
    accountEmail: record.accountEmail,
    accountName: record.accountName,
    route: record.route,
    model: record.model,
    requestedModel: record.requestedModel,
    upstreamModel: record.upstreamModel,
    serviceTier: record.serviceTier,
    statusCode: record.statusCode,
    clientTransport: record.clientTransport,
    upstreamTransport: record.upstreamTransport,
    attemptIndex: record.attemptIndex,
    attemptCount: record.attemptCount,
    responseId: record.responseId,
    upstreamRequestId: record.upstreamRequestId,
    protocol: record.protocol,
    httpVersion: record.httpVersion,
    clientStatusCode: record.clientStatusCode,
    upstreamStatusCode: record.upstreamStatusCode,
    websocketPool: record.websocketPool,
    imageGenerationRequested: record.imageGenerationRequested,
    imageGenerationSucceeded: record.imageGenerationSucceeded,
    latencyMs: record.latencyMs,
    firstTokenLatencyMs: record.firstTokenLatencyMs,
    latencyDetails: record.latencyDetails,
    inputTokens: record.inputTokens,
    outputTokens: record.outputTokens,
    cachedTokens: record.cachedTokens,
    cacheWriteTokens: record.cacheWriteTokens,
    reasoningTokens: record.reasoningTokens,
    imageInputTokens: record.imageInputTokens,
    imageOutputTokens: record.imageOutputTokens,
    message: record.message,
    createdAt: record.createdAt,
    createdAtDisplay: record.createdAtDisplay,
    clientIp: record.clientIp,
    userAgent: record.userAgent,
    reasoningEffort: record.reasoningEffort,
    reasoningPreset: record.reasoningPreset,
    compact: record.compact === true,
    requestKind: record.requestKind,
    subagentKind: record.subagentKind,
    tokenDetails: record.tokenDetails,
    billing: record.billing,
    costs: record.costs,
    costCoverage: record.costCoverage,
    firstTokenLatencyMsDisplay: record.firstTokenLatencyMsDisplay,
    latencyMsDisplay: record.latencyMsDisplay,
    logicalOutcome: record.logicalOutcome,
    providerMetadata: metadata,
    requestBody: metadata.requestBody,
    responseBody: metadata.responseBody,
    attempts: record.attempts,
    attemptsComplete: record.attemptsComplete,
  }
}

export type UsageDisplayRecord = UsageListRecord

type UsageCommonRecord = UsageDisplayRecord | UsageViewModel

export function usageTransportType(transport?: string | null) {
  if (transport === 'websocket')
    return 'WS'

  if (transport === 'http_sse')
    return 'SSE'
  if (transport === 'http' || transport === 'http_json')
    return 'HTTP'
  return transport || '—'
}

export function usageTransportTypeClass(transport?: string | null) {
  const type = usageTransportType(transport)
  if (type === 'WS')
    return 'bg-cp-blue-bg text-cp-blue-text-on-bg'
  if (type === 'SSE')
    return 'bg-cp-green-bg text-cp-green-text-on-bg'
  return 'bg-cp-fill-tertiary text-cp-text-secondary'
}

export function usageAccountText(record: UsageCommonRecord) {
  return record.accountEmail || record.accountName || record.accountId || '—'
}

export function usageAuthenticationKind(record: UsageCommonRecord) {
  return typeof record.authenticationKind === 'string' ? record.authenticationKind : null
}

export function usageClientIp(record: UsageCommonRecord) {
  return record.clientIp || '—'
}

export function usageUserAgent(record: UsageCommonRecord) {
  return record.userAgent || '—'
}

export function usageReasoningEffort(record: UsageCommonRecord) {
  const reasoningEffort = record.reasoningEffort || '—'
  if (usageIsSubagent(record))
    return reasoningEffort
  return record.reasoningPreset || reasoningEffort
}

export function usageIsSubagent(record: UsageCommonRecord) {
  return Boolean(record.subagentKind)
}

export function usageIsCompact(record: UsageCommonRecord) {
  return record.compact === true
}

export function usageModelDisplay(record: UsageCommonRecord) {
  const requestedModel = record.requestedModel || ''
  const upstreamModel = record.upstreamModel || ''
  const storedModel = record.model || ''
  const primary = requestedModel || storedModel || upstreamModel || '—'
  const secondary
    = upstreamModel && upstreamModel !== primary
      ? upstreamModel
      : requestedModel && storedModel && storedModel !== requestedModel
        ? storedModel
        : ''

  return { primary, secondary }
}

export function usageTokenDetails(record: UsageCommonRecord) {
  return record.tokenDetails
}

export function usageLatencyDetails(record: UsageCommonRecord) {
  const latencyDetails = record.latencyDetails
  const firstTokenMs = durationValue(
    record.firstTokenLatencyMs ?? latencyDetails?.firstTokenMs,
  )
  const firstEventMs = durationValue(latencyDetails?.firstEventMs)
  const totalMs = durationValue(record.latencyMs)
  const firstReasoningMs = durationValue(latencyDetails?.firstReasoningMs)
  const firstTextMs = durationValue(latencyDetails?.firstTextMs)
  const breakdownItems = []

  if (firstTokenMs !== null && totalMs !== null && firstTokenMs <= totalMs) {
    breakdownItems.push({ label: '首字等待', value: formatDuration(firstTokenMs) })

    if (firstTextMs !== null && firstTextMs >= firstTokenMs && firstTextMs <= totalMs) {
      const beforeTextMs = firstTextMs - firstTokenMs
      if (beforeTextMs > 0) {
        breakdownItems.push({
          label: firstReasoningMs === firstTokenMs ? '推理到正文' : '首个输出到正文',
          value: formatDuration(beforeTextMs),
        })
      }
      breakdownItems.push({ label: '正文生成', value: formatDuration(totalMs - firstTextMs) })
    }
    else {
      breakdownItems.push({
        label: '首个输出后完成',
        value: formatDuration(totalMs - firstTokenMs),
      })
    }
  }

  const transportItems = [
    { label: '准入判定', value: durationValue(latencyDetails?.admissionDecisionMs) },
    { label: '账号选择等待', value: durationValue(latencyDetails?.accountSelectionWaitMs) },
    {
      label: '传输决策等待',
      value: durationValue(latencyDetails?.transportDecisionWaitMs),
    },
    { label: 'WebSocket 连接', value: durationValue(latencyDetails?.wsConnectMs) },
    { label: '上游响应头', value: durationValue(latencyDetails?.upstreamHeadersMs) },
    { label: '首个上游事件', value: firstEventMs },
    { label: '上游处理', value: durationValue(latencyDetails?.openaiProcessingMs) },
  ]
    .filter(item => item.value !== null)
    .map(item => ({ ...item, value: formatDuration(item.value) }))

  if (
    latencyDetails?.capacityUsedSlots != null
    && latencyDetails.capacityTotalSlots != null
  ) {
    transportItems.push({
      label: '账号槽位快照',
      value: `${latencyDetails.capacityUsedSlots} / ${latencyDetails.capacityTotalSlots}`,
    })
  }

  return {
    // SSE 可能先收到生命周期事件而没有文本或推理增量；这不是首字，须保留原始语义。
    firstOutputLabel: firstTokenMs === null && firstEventMs !== null ? '首事件' : '首字',
    firstOutputDisplay: formatDuration(firstTokenMs ?? firstEventMs),
    totalDisplay: formatDuration(totalMs),
    breakdownItems,
    transportItems,
  }
}

export function usageBilling(record: UsageCommonRecord) {
  return record.billing
}

export function usageBillingText(record: UsageCommonRecord) {
  return usageBilling(record)?.totalAmountDisplay || '—'
}

export function visibleRequestText(record: UsageViewModel) {
  const body = record.requestBody
  if (!body)
    return ''

  return extractInputText(body) || JSON.stringify(body, null, 2)
}

export function visibleResponseText(record: UsageViewModel) {
  const body = record.responseBody
  if (!body)
    return ''

  if (typeof body === 'string')
    return body

  return stringProperty(asRecord(body), 'output_text') || extractOutputText(body) || JSON.stringify(body, null, 2)
}

function durationValue(value: unknown) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null
}

function extractInputText(body: unknown) {
  const input = property(asRecord(body), 'input')
  if (typeof input === 'string')
    return input

  if (!Array.isArray(input))
    return ''

  return input
    .flatMap((item) => {
      const content = property(asRecord(item), 'content')
      if (typeof content === 'string')
        return [content]

      if (!Array.isArray(content))
        return []

      return content.flatMap((part) => {
        const value = asRecord(part)
        const text = stringProperty(value, 'text')
        return value?.type === 'input_text' && text ? [text] : []
      })
    })
    .filter(Boolean)
    .join('\n')
}

function extractOutputText(body: unknown) {
  const output = property(asRecord(body), 'output')
  if (!Array.isArray(output))
    return ''

  return output
    .flatMap((item) => {
      const content = property(asRecord(item), 'content')
      if (!Array.isArray(content))
        return []
      return content.flatMap((part) => {
        const text = stringProperty(asRecord(part), 'text')
        return text ? [text] : []
      })
    })
    .filter(Boolean)
    .join('\n')
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return isRecord(value) ? value : undefined
}

function property(value: Record<string, unknown> | undefined, key: string) {
  return value?.[key]
}

function stringProperty(value: Record<string, unknown> | undefined, key: string) {
  const valueAtKey = property(value, key)
  return typeof valueAtKey === 'string' ? valueAtKey : undefined
}
