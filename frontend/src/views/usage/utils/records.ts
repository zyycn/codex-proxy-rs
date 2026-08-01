// Usage 记录的单一规范化 view model 消费层。
//
// 组件只消费 `UsageDisplayRecord`（即 `UsageViewModel`）；兼容性回退只存在于
// `normalizeUsageRecord`（api/modules/usage.ts），组件不再逐字段 fallback。

import type { UsageViewModel } from '@/api'

import { formatDuration } from './format'

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
    return 'bg-(--cp-info-bg) text-(--cp-info-text)'
  if (type === 'SSE')
    return 'bg-(--cp-success-bg) text-(--cp-success-text)'
  if (type === 'Chat')
    return 'bg-(--cp-warning-bg) text-(--cp-warning-text)'
  return 'bg-(--cp-bg-subtle) text-(--cp-text-secondary)'
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
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined
}

function property(value: Record<string, unknown> | undefined, key: string) {
  return value?.[key]
}

function stringProperty(value: Record<string, unknown> | undefined, key: string) {
  const valueAtKey = property(value, key)
  return typeof valueAtKey === 'string' ? valueAtKey : undefined
}
