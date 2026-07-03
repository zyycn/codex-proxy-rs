export const usageRecordColumns = [
  {
    key: 'accountEmail',
    label: '账号',
    width: '250px',
    fixed: 'left' as const,
    ellipsis: false,
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
    width: '172px',
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
    key: 'costDetails',
    label: '费用',
    width: '132px',
    align: 'right' as const,
    ellipsis: false,
  },
  {
    key: 'firstTokenLatencyMsDisplay',
    label: '首 Token',
    width: '104px',
    align: 'right' as const,
    ellipsis: false,
    cellClass: 'font-mono text-[12px] font-[650] tabular-nums text-(--cp-text-secondary)',
  },
  {
    key: 'latencyMsDisplay',
    label: '耗时',
    width: '96px',
    align: 'right' as const,
    ellipsis: false,
    cellClass: 'font-mono text-[12px] font-[650] tabular-nums text-(--cp-text-secondary)',
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
    width: '146px',
    ellipsis: false,
  },
  {
    key: 'userAgent',
    label: 'User-Agent',
    width: '300px',
    ellipsis: false,
    cellClass:
      'whitespace-normal break-words text-[12px] leading-[1.45] font-[650] text-(--cp-text-secondary)',
  },
  {
    key: 'actions',
    label: '操作',
    width: '92px',
    ellipsis: false,
    fixed: 'right' as const,
    headerClass: '!px-4',
    cellClass: '!px-4',
  },
]

export const levelColors: Record<string, { bg: string; text: string }> = {
  info: { bg: 'bg-(--cp-success-bg)', text: 'text-(--cp-success-text)' },
  error: { bg: 'bg-(--cp-danger-bg)', text: 'text-(--cp-danger-text)' },
}

export const statusOptions = [
  { label: '全部记录', value: '' },
  { label: '正常', value: 'info' },
  { label: '错误', value: 'error' },
]

export const usageTimeRangeOptions = [
  { label: '今天', value: 'today' },
  { label: '最近 7 天', value: '7d' },
  { label: '最近 30 天', value: '30d' },
  { label: '全部时间', value: 'all' },
]

export const levelLabels: Record<string, string> = {
  info: '正常',
  error: '错误',
}

export function tokenTotal(record: any) {
  const tokenDetails = record?.tokenDetails
  if (tokenDetails) {
    return numberValue(tokenDetails.totalTokens)
  }

  const usage = record?.metadata?.usage
  if (!usage) {
    return 0
  }

  return (
    numberValue(usage.totalTokens) ||
    numberValue(usage.inputTokens) + numberValue(usage.outputTokens)
  )
}

export function formatTokenCount(value: number) {
  if (!value) {
    return '—'
  }

  return new Intl.NumberFormat('zh-CN').format(value)
}

export function formatUsageMetric(value: number) {
  return new Intl.NumberFormat('zh-CN').format(value || 0)
}

export function formatCompactUsageMetric(value: number) {
  const num = Number(value || 0)
  if (num >= 1_000_000_000) return `${formatCompact(num / 1_000_000_000)}B`
  if (num >= 1_000_000) return `${formatCompact(num / 1_000_000)}M`
  if (num >= 1_000) return `${formatCompact(num / 1_000)}K`
  return new Intl.NumberFormat('zh-CN').format(num)
}

function formatCompact(value: number) {
  const rounded =
    value >= 100 ? value.toFixed(0) : value >= 10 ? value.toFixed(1) : value.toFixed(2)
  return rounded.replace(/\.0+$/, '').replace(/(\.\d*[1-9])0+$/, '$1')
}

export function formatLatencyAverage(value: number | null) {
  if (value === null || value === undefined) {
    return '—'
  }

  return `${Math.round(value)} ms`
}

export function formatCostMetric(value: number) {
  if (!value) {
    return '$0.000000'
  }

  return `$${Number(value || 0).toFixed(6)}`
}

export function usageRecordType(record: any) {
  if (record?.transport === 'websocket') {
    return 'WS'
  }

  if (record?.metadata?.stream === true || record?.transport === 'http_sse') {
    return 'SSE'
  }

  if (record?.metadata?.stream === false) {
    return 'HTTP'
  }

  return record?.metadata?.apiKind === 'chat' ? 'Chat' : 'HTTP'
}

export function usageRecordTypeClass(record: any) {
  const type = usageRecordType(record)
  if (type === 'WS') return 'bg-(--cp-info-bg) text-(--cp-info-text)'
  if (type === 'SSE') return 'bg-(--cp-success-bg) text-(--cp-success-text)'
  if (type === 'Chat') return 'bg-(--cp-warning-bg) text-(--cp-warning-text)'
  return 'bg-(--cp-bg-subtle) text-(--cp-text-secondary)'
}

export function usageAccountText(record: any) {
  return record?.accountEmail || '—'
}

export function usageClientIp(record: any) {
  return record?.clientIp || record?.metadata?.clientIp || '—'
}

export function usageUserAgent(record: any) {
  return record?.userAgent || record?.metadata?.userAgent || '—'
}

export function usageReasoningEffort(record: any) {
  return record?.reasoningEffort || record?.metadata?.reasoningEffort || '—'
}

export function usageModelDisplay(record: any) {
  const requestedModel = record?.requestedModel || record?.metadata?.requestedModel || ''
  const upstreamModel = record?.upstreamModel || record?.metadata?.upstreamModel || ''
  const storedModel = record?.model || ''
  const primary = requestedModel || storedModel || upstreamModel || '—'
  const secondary =
    upstreamModel && upstreamModel !== primary
      ? upstreamModel
      : requestedModel && storedModel && storedModel !== requestedModel
        ? storedModel
        : ''

  return { primary, secondary }
}

export function usageTokenDetails(record: any) {
  const details = record?.tokenDetails
  if (details) {
    return details
  }

  const usage = record?.metadata?.usage || {}
  const inputTokens = numberValue(usage.inputTokens)
  const outputTokens = numberValue(usage.outputTokens)
  const cachedTokens = numberValue(usage.cachedTokens)
  const reasoningTokens = numberValue(usage.reasoningTokens)
  const totalTokens = numberValue(usage.totalTokens) || inputTokens + outputTokens

  return {
    inputTokens,
    outputTokens,
    cachedTokens,
    reasoningTokens,
    totalTokens,
    inputTokensDisplay: formatTokenCount(inputTokens),
    outputTokensDisplay: formatTokenCount(outputTokens),
    cachedTokensDisplay: formatCompactTokenCount(cachedTokens),
    reasoningTokensDisplay: formatTokenCount(reasoningTokens),
    totalTokensDisplay: formatTokenCount(totalTokens),
  }
}

export function usageCostDetails(record: any) {
  return record?.costDetails || null
}

export function usageCostText(record: any) {
  return usageCostDetails(record)?.totalCostDisplay || '—'
}

function formatCompactTokenCount(value: number) {
  if (!value) return '—'
  if (value < 1000) return formatTokenCount(value)
  if (value < 1_000_000) return `${Number((value / 1000).toFixed(value >= 100_000 ? 0 : 1))}K`
  return `${Number((value / 1_000_000).toFixed(value >= 100_000_000 ? 0 : 1))}M`
}

export function visibleRequestText(record: any) {
  const body = record?.metadata?.requestBody
  if (!body) {
    return ''
  }

  return extractInputText(body) || JSON.stringify(body, null, 2)
}

export function visibleResponseText(record: any) {
  const body = record?.metadata?.responseBody
  if (!body) {
    return ''
  }

  if (typeof body === 'string') {
    return body
  }

  return body.output_text || extractOutputText(body) || JSON.stringify(body, null, 2)
}

function numberValue(value: unknown) {
  return typeof value === 'number' && Number.isFinite(value) ? value : 0
}

function extractInputText(body: any) {
  const input = body?.input
  if (typeof input === 'string') {
    return input
  }

  if (!Array.isArray(input)) {
    return ''
  }

  return input
    .flatMap((item) => {
      if (typeof item?.content === 'string') {
        return [item.content]
      }

      if (!Array.isArray(item?.content)) {
        return []
      }

      return item.content
        .filter((part: any) => part?.type === 'input_text' && typeof part.text === 'string')
        .map((part: any) => part.text)
    })
    .filter(Boolean)
    .join('\n')
}

function extractOutputText(body: any) {
  if (!Array.isArray(body?.output)) {
    return ''
  }

  return body.output
    .flatMap((item: any) =>
      Array.isArray(item?.content)
        ? item.content
            .filter((part: any) => typeof part?.text === 'string')
            .map((part: any) => part.text)
        : [],
    )
    .filter(Boolean)
    .join('\n')
}
