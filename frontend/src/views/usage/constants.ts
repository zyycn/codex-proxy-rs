import type { getDashboardSummary, getUsageRecords } from '@/api'
import { formatDuration } from './utils/format'

type UsageRecord = Awaited<ReturnType<typeof getUsageRecords>>['items'][number]
type DashboardUsageRecord = Awaited<ReturnType<typeof getDashboardSummary>>['usageRecords'][number]
export type UsageDisplayRecord = UsageRecord | DashboardUsageRecord

export const usageRecordColumns = [
  {
    key: 'accountEmail',
    label: '账号',
    width: '250px',
    ellipsis: true,
  },
  {
    key: 'model',
    label: '模型',
    width: '160px',
    ellipsis: false,
  },
  {
    key: 'reasoningEffort',
    label: '推理强度',
    width: '98px',
    ellipsis: false,
    cellClass: 'whitespace-nowrap text-[12px] font-[700] text-(--cp-text-primary)',
  },
  {
    key: 'route',
    label: '端点',
    width: '185px',
    ellipsis: false,
    cellClass: 'font-mono text-[12px] font-[650]',
  },
  {
    key: 'recordType',
    label: '类型',
    width: '78px',
    align: 'center' as const,
    ellipsis: false,
  },
  {
    key: 'tokenDetails',
    label: 'TOKEN',
    width: '184px',
    align: 'right' as const,
    ellipsis: false,
  },
  {
    key: 'billing',
    label: '费用',
    width: '132px',
    align: 'right' as const,
    ellipsis: false,
  },
  {
    key: 'latency',
    label: '延迟',
    width: '156px',
    align: 'right' as const,
    ellipsis: false,
  },
  {
    key: 'createdAtDisplay',
    label: '时间',
    width: '190px',
    ellipsis: false,
    cellClass:
      'whitespace-nowrap font-mono text-[12px] font-[650] tabular-nums text-(--cp-text-secondary)',
  },
  {
    key: 'clientIp',
    label: 'IP',
    width: '240px',
    ellipsis: false,
  },
  {
    key: 'userAgent',
    label: 'User-Agent',
    width: '340px',
    ellipsis: false,
    cellClass:
      'whitespace-normal break-words text-[12px] leading-[1.45] font-[650] text-(--cp-text-secondary)',
  },
  {
    key: 'actions',
    label: '操作',
    width: '92px',
    ellipsis: false,
    headerClass: '!px-4',
    cellClass: '!px-4',
  },
]

export const opsErrorColumns = [
  {
    key: 'createdAtDisplay',
    label: '时间',
    width: '190px',
    cellClass:
      'whitespace-nowrap font-mono text-[12px] font-[650] tabular-nums text-(--cp-text-secondary)',
  },
  { key: 'statusCode', label: '状态码', width: '96px', align: 'center' as const },
  {
    key: 'failureClass',
    label: '失败分类',
    width: '170px',
    cellClass: 'font-mono text-[12px] font-[650]',
  },
  {
    key: 'kind',
    label: '事件',
    width: '170px',
    cellClass: 'font-mono text-[12px] font-[650]',
  },
  {
    key: 'route',
    label: '端点',
    width: '190px',
    cellClass: 'font-mono text-[12px] font-[650]',
  },
  {
    key: 'model',
    label: '模型',
    width: '180px',
    cellClass: 'font-mono text-[12px] font-[650]',
  },
  {
    key: 'accountId',
    label: '账号 ID',
    width: '230px',
    cellClass: 'font-mono text-[12px] font-[650]',
  },
  {
    key: 'requestId',
    label: '请求 ID',
    width: '250px',
    cellClass: 'font-mono text-[12px] font-[650]',
  },
  { key: 'message', label: '消息', minWidth: '300px', flex: 1 },
  {
    key: 'actions',
    label: '操作',
    width: '80px',
    headerClass: '!px-4',
    cellClass: '!px-4',
  },
]

export const usageTimeRangeOptions = [
  { label: '今天', value: 'today' },
  { label: '最近 7 天', value: '7d' },
  { label: '最近 30 天', value: '30d' },
]

export function usageRecordType(record: UsageDisplayRecord) {
  const metadata = recordMetadata(record)
  if (record?.transport === 'websocket') {
    return 'WS'
  }

  if (metadata?.stream === true || record.transport === 'http_sse') {
    return 'SSE'
  }

  if (metadata?.stream === false) {
    return 'HTTP'
  }

  return metadata?.apiKind === 'chat' ? 'Chat' : 'HTTP'
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
  return record.accountEmail || '—'
}

export function usageClientIp(record: UsageDisplayRecord) {
  return record.clientIp || stringProperty(recordMetadata(record), 'clientIp') || '—'
}

export function usageUserAgent(record: UsageDisplayRecord) {
  return record.userAgent || stringProperty(recordMetadata(record), 'userAgent') || '—'
}

export function usageReasoningEffort(record: UsageDisplayRecord) {
  const metadata = recordMetadata(record)
  const reasoningEffort
    = record.reasoningEffort || stringProperty(metadata, 'reasoningEffort') || '—'
  if (usageIsSubagent(record)) {
    return reasoningEffort
  }

  return record.reasoningPreset || stringProperty(metadata, 'reasoningPreset') || reasoningEffort
}

export function usageIsSubagent(record: UsageDisplayRecord) {
  return Boolean(record.subagentKind)
}

export function usageIsCompact(record: UsageDisplayRecord) {
  return record.compact === true || recordMetadata(record)?.compact === true
}

export function usageModelDisplay(record: UsageDisplayRecord) {
  const metadata = recordMetadata(record)
  const requestedModel = record.requestedModel || stringProperty(metadata, 'requestedModel') || ''
  const upstreamModel = record.upstreamModel || stringProperty(metadata, 'upstreamModel') || ''
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
  const latencyDetails = recordLatencyDetails(record)
  const firstTokenMs = durationValue(
    record.firstTokenLatencyMs ?? property(latencyDetails, 'firstTokenMs'),
  )
  const totalMs = durationValue(record.latencyMs)
  const firstReasoningMs = durationValue(property(latencyDetails, 'firstReasoningMs'))
  const firstTextMs = durationValue(property(latencyDetails, 'firstTextMs'))
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
      value: durationValue(property(latencyDetails, 'transportDecisionWaitMs')),
    },
    { label: 'WebSocket 连接', value: durationValue(property(latencyDetails, 'wsConnectMs')) },
    { label: '上游响应头', value: durationValue(property(latencyDetails, 'upstreamHeadersMs')) },
    { label: '首个上游事件', value: durationValue(property(latencyDetails, 'firstEventMs')) },
    { label: '上游处理', value: durationValue(property(latencyDetails, 'openaiProcessingMs')) },
  ]
    .filter(item => item.value !== null)
    .map(item => ({ ...item, value: formatDuration(item.value) }))

  return {
    firstTokenDisplay: formatDuration(firstTokenMs),
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
  const body = property(recordMetadata(record), 'requestBody')
  if (!body) {
    return ''
  }

  return extractInputText(body) || JSON.stringify(body, null, 2)
}

export function visibleResponseText(record: UsageDisplayRecord) {
  const body = property(recordMetadata(record), 'responseBody')
  if (!body) {
    return ''
  }

  if (typeof body === 'string') {
    return body
  }

  return stringProperty(asRecord(body), 'output_text') || extractOutputText(body) || JSON.stringify(body, null, 2)
}

function durationValue(value: unknown) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null
}

function extractInputText(body: unknown) {
  const input = property(asRecord(body), 'input')
  if (typeof input === 'string') {
    return input
  }

  if (!Array.isArray(input)) {
    return ''
  }

  return input
    .flatMap((item) => {
      const content = property(asRecord(item), 'content')
      if (typeof content === 'string') {
        return [content]
      }

      if (!Array.isArray(content)) {
        return []
      }

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
  if (!Array.isArray(output)) {
    return ''
  }

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

function recordMetadata(record: UsageDisplayRecord) {
  return asRecord('metadata' in record ? record.metadata : undefined)
}

function recordLatencyDetails(record: UsageDisplayRecord) {
  return asRecord('latencyDetails' in record ? record.latencyDetails : undefined)
    ?? recordMetadata(record)
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
