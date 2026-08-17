// Usage 记录的单一规范化 view model 消费层。
//
// 组件只消费 `UsageDisplayRecord`（即 `UsageViewModel`）；兼容性回退只在本文件发生。

import type {
  UsageAttempt,
  UsageBilling,
  UsageCost,
  UsageCostCoverage,
  UsageLatencyDetails,
  UsageRecord,
  UsageRecordDetail,
  UsageTokenDetails,
} from '@/api'

import { isRecord } from '@/utils/object'
import { formatDuration } from './format'

// Usage 记录的规范化 view model：组件只消费这个形状，兼容性回退只发生在本文件。
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
  model: string
  requestedModel: string | null
  upstreamModel: string | null
  serviceTier: string | null
  statusCode: number | null
  transport: string | null
  stream: boolean | null
  apiKind: string | null
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

const CORE_METADATA_KEYS = new Set([
  'protocol',
  'logicalOutcome',
  'attemptCount',
  'requestedModel',
  'upstreamModel',
  'clientIp',
  'userAgent',
  'reasoningEffort',
  'reasoningPreset',
  'compact',
  'requestKind',
  'subagentKind',
  'transport',
  'httpVersion',
  'clientStatusCode',
  'upstreamStatusCode',
  'responseId',
  'upstreamRequestId',
  'websocketPool',
  'imageGenerationRequested',
  'imageGenerationSucceeded',
  'latencyDetails',
])

function metadataRecord(value: unknown): Record<string, unknown> {
  return isRecord(value) ? value : {}
}

/** 一次性兼容 normalize：顶层 Core 字段优先，旧响应中的 metadata 回退只发生在这里。 */
export function normalizeUsageRecord(record: UsageRecord | UsageRecordDetail): UsageViewModel {
  const metadata = metadataRecord(record.metadata)
  const legacy = <T>(top: T | null | undefined, key: string): T | null | undefined =>
    top ?? (metadata[key] as T | undefined)

  const latencyDetails = legacy(record.latencyDetails, 'latencyDetails') ?? null
  const providerMetadata: Record<string, unknown> = {}
  for (const [key, value] of Object.entries(metadata)) {
    if (!CORE_METADATA_KEYS.has(key))
      providerMetadata[key] = value
  }

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
    requestedModel: legacy(record.requestedModel, 'requestedModel') ?? null,
    upstreamModel: legacy(record.upstreamModel, 'upstreamModel') ?? null,
    serviceTier: record.serviceTier,
    statusCode: record.statusCode,
    transport: legacy(record.transport, 'transport') ?? null,
    stream: typeof metadata.stream === 'boolean' ? metadata.stream : null,
    apiKind: typeof metadata.apiKind === 'string' ? metadata.apiKind : null,
    attemptIndex: record.attemptIndex,
    attemptCount: legacy(record.attemptCount, 'attemptCount') ?? 0,
    responseId: legacy(record.responseId, 'responseId') ?? null,
    upstreamRequestId: legacy(record.upstreamRequestId, 'upstreamRequestId') ?? null,
    protocol: legacy(record.protocol, 'protocol') ?? '',
    httpVersion: legacy(record.httpVersion, 'httpVersion') ?? null,
    clientStatusCode: legacy(record.clientStatusCode, 'clientStatusCode') ?? null,
    upstreamStatusCode: legacy(record.upstreamStatusCode, 'upstreamStatusCode') ?? null,
    websocketPool: legacy(record.websocketPool, 'websocketPool') ?? null,
    imageGenerationRequested:
      legacy(record.imageGenerationRequested, 'imageGenerationRequested') ?? false,
    imageGenerationSucceeded:
      legacy(record.imageGenerationSucceeded, 'imageGenerationSucceeded') ?? null,
    latencyMs: record.latencyMs,
    firstTokenLatencyMs: record.firstTokenLatencyMs,
    latencyDetails,
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
    clientIp: legacy(record.clientIp, 'clientIp') ?? null,
    userAgent: legacy(record.userAgent, 'userAgent') ?? null,
    reasoningEffort: legacy(record.reasoningEffort, 'reasoningEffort') ?? null,
    reasoningPreset: legacy(record.reasoningPreset, 'reasoningPreset') ?? null,
    compact: legacy(record.compact, 'compact') ?? false,
    requestKind: legacy(record.requestKind, 'requestKind') ?? null,
    subagentKind: legacy(record.subagentKind, 'subagentKind') ?? null,
    tokenDetails: record.tokenDetails,
    billing: record.billing,
    costs: record.costs,
    costCoverage: record.costCoverage,
    firstTokenLatencyMsDisplay: record.firstTokenLatencyMsDisplay,
    latencyMsDisplay: record.latencyMsDisplay,
    logicalOutcome: legacy(record.logicalOutcome, 'logicalOutcome') ?? '',
    providerMetadata,
    requestBody: metadata.requestBody,
    responseBody: metadata.responseBody,
    attempts: 'attempts' in record ? record.attempts : undefined,
    attemptsComplete: 'attemptsComplete' in record ? record.attemptsComplete : undefined,
  }
}

export type UsageDisplayRecord = UsageViewModel

export function usageRecordType(record: UsageDisplayRecord) {
  if (record.transport === 'websocket')
    return 'WS'

  if (record.stream === true || record.transport === 'http_sse')
    return 'SSE'

  if (record.stream === false)
    return 'HTTP'

  return record.apiKind === 'chat' ? 'Chat' : 'HTTP'
}

export function usageRecordTypeClass(record: UsageDisplayRecord) {
  const type = usageRecordType(record)
  if (type === 'WS')
    return 'bg-cp-info-bg text-cp-info-text'
  if (type === 'SSE')
    return 'bg-cp-success-bg text-cp-success-text'
  if (type === 'Chat')
    return 'bg-cp-warning-bg text-cp-warning-text'
  return 'bg-cp-subtle text-cp-secondary'
}

export function usageAccountText(record: UsageDisplayRecord) {
  return record.accountEmail || record.accountName || record.accountId || '—'
}

export function usageAuthenticationKind(record: UsageDisplayRecord) {
  return typeof record.authenticationKind === 'string' ? record.authenticationKind : null
}

export function usageClientIp(record: UsageDisplayRecord) {
  return record.clientIp || '—'
}

export function usageUserAgent(record: UsageDisplayRecord) {
  return record.userAgent || '—'
}

export function usageReasoningEffort(record: UsageDisplayRecord) {
  const reasoningEffort = record.reasoningEffort || '—'
  if (usageIsSubagent(record))
    return reasoningEffort
  return record.reasoningPreset || reasoningEffort
}

export function usageIsSubagent(record: UsageDisplayRecord) {
  return Boolean(record.subagentKind)
}

export function usageIsCompact(record: UsageDisplayRecord) {
  return record.compact === true
}

export function usageModelDisplay(record: UsageDisplayRecord) {
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

export function usageTokenDetails(record: UsageDisplayRecord) {
  return record.tokenDetails
}

export function usageLatencyDetails(record: UsageDisplayRecord) {
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

  return {
    // SSE 可能先收到生命周期事件而没有文本或推理增量；这不是首字，须保留原始语义。
    firstOutputLabel: firstTokenMs === null && firstEventMs !== null ? '首事件' : '首字',
    firstOutputDisplay: formatDuration(firstTokenMs ?? firstEventMs),
    totalDisplay: formatDuration(totalMs),
    breakdownItems,
    transportItems,
  }
}

export function usageBilling(record: UsageDisplayRecord) {
  return record.billing
}

export function usageBillingText(record: UsageDisplayRecord) {
  return usageBilling(record)?.totalAmountDisplay || '—'
}

export function visibleRequestText(record: UsageDisplayRecord) {
  const body = record.requestBody
  if (!body)
    return ''

  return extractInputText(body) || JSON.stringify(body, null, 2)
}

export function visibleResponseText(record: UsageDisplayRecord) {
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
