import type { UsageListRecord, UsageRecordDetail, UsageTokenDetails } from '@/api'
import { formatCompactNumber, formatDuration, formatInteger } from '@/utils/format'

// 列表与详情使用独立读模型，展示函数只依赖两者的公共字段。
type UsageCommonRecord = UsageListRecord | UsageRecordDetail
type UsageLatencyRecord = Pick<UsageCommonRecord, 'latencyDetails' | 'firstTokenLatencyMs' | 'latencyMs'>

export type UsagePerformanceRecord = Pick<UsageCommonRecord, 'latencyDetails' | 'firstTokenLatencyMs'> & {
  tokenDetails: Pick<UsageListRecord['tokenDetails'], 'outputTokens'> | null
}

interface UsageTimingItem {
  label: string
  value: string
  emphasized?: boolean
}

interface UsageTimingSection {
  title: string
  source: 'official' | 'local'
  items: UsageTimingItem[]
}

export function usageFreshInputTokens(input: number, cached: number, written: number) {
  // 输入总量包含缓存读写，普通输入与缓存明细分开展示
  return Math.max(0, input - cached - written)
}

export function usageTokenDetails(details: UsageTokenDetails): UsageTokenDetails {
  const inputTokens = details.inputTokens === null
    ? null
    : usageFreshInputTokens(details.inputTokens, details.cachedInputTokens ?? 0, details.cacheWriteInputTokens ?? 0)

  return {
    ...details,
    inputTokens,
    inputTokensDisplay: inputTokens === null ? details.inputTokensDisplay : formatInteger(inputTokens),
  }
}

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
    return 'bg-cp-blue-container text-cp-blue-on-container'
  if (type === 'SSE')
    return 'bg-cp-green-container text-cp-green-on-container'
  return 'bg-cp-fill-tertiary text-cp-text-secondary'
}

export function usageAccountText(record: UsageCommonRecord) {
  return record.accountEmail || record.accountName || record.accountId || '—'
}

export function usageClientIp(record: { clientIp?: string | null }) {
  return record.clientIp || '—'
}

export function usageUserAgent(record: { userAgent?: string | null }) {
  return record.userAgent || '—'
}

export function usageReasoningEffort(record: UsageCommonRecord) {
  const reasoningEffort = record.reasoningEffort || '—'
  if (record.subagentKind)
    return reasoningEffort
  return record.reasoningPreset || reasoningEffort
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

  const responseModel = record.upstreamResponseModel || ''
  const returned = responseModel && responseModel !== primary ? responseModel : ''
  const routes = []
  if (secondary && secondary !== returned) {
    routes.push({
      model: secondary,
      kind: 'mapped' as const,
      description: `网关映射后发送给上游的模型：${secondary}`,
    })
  }
  if (returned) {
    routes.push({
      model: returned,
      kind: 'returned' as const,
      description: returned === secondary
        ? `上游返回模型：${returned}（与网关映射后发送的模型一致）`
        : `上游返回模型：${returned}`,
    })
  }

  return { primary, secondary, routes }
}

export function usagePerformanceDetails(record: UsagePerformanceRecord) {
  const upstreamMs = upstreamResponseMs(record.latencyDetails)
  const outputTokens = record.tokenDetails?.outputTokens
  // 输出已包含推理 Token，分母只取同一完成响应的官方时间跨度
  const throughput = typeof outputTokens === 'number' && Number.isFinite(outputTokens) && outputTokens > 0
    && upstreamMs !== null
    ? outputTokens * 1000 / upstreamMs
    : null

  return {
    throughputDisplay: throughput === null ? '—' : `${formatCompactNumber(throughput)} tok/s`,
    firstTokenDisplay: formatDuration(durationValue(record.firstTokenLatencyMs)),
  }
}

export function usageLatencyDetails(record: UsageLatencyRecord) {
  const latencyDetails = record.latencyDetails
  const firstTokenMs = durationValue(record.firstTokenLatencyMs)
  const upstreamMs = upstreamResponseMs(latencyDetails)
  const totalMs = durationValue(record.latencyMs)
  const firstReasoningMs = durationValue(latencyDetails?.firstReasoningMs)
  const firstTextMs = durationValue(latencyDetails?.firstTextMs)
  const requestItems: UsageTimingItem[] = []

  if (firstTokenMs !== null && totalMs !== null && firstTokenMs <= totalMs) {
    requestItems.push({ label: '首个输出等待', value: formatDuration(firstTokenMs) })

    if (firstTextMs !== null && firstTextMs >= firstTokenMs && firstTextMs <= totalMs) {
      const beforeTextMs = firstTextMs - firstTokenMs
      if (beforeTextMs > 0) {
        requestItems.push({
          label: firstReasoningMs === firstTokenMs ? '推理到正文' : '首个输出到正文',
          value: formatDuration(beforeTextMs),
        })
      }
      requestItems.push({ label: '正文到完成', value: formatDuration(totalMs - firstTextMs) })
    }
    else {
      requestItems.push({
        label: '首个输出后完成',
        value: formatDuration(totalMs - firstTokenMs),
      })
    }
  }

  requestItems.push({ label: '总耗时', value: formatDuration(totalMs), emphasized: true })

  const transportItems: UsageTimingItem[] = [
    { label: '准入判定', value: durationValue(latencyDetails?.admissionDecisionMs) },
    { label: '账号选择等待', value: durationValue(latencyDetails?.accountSelectionWaitMs) },
    {
      label: '传输决策等待',
      value: durationValue(latencyDetails?.transportDecisionWaitMs),
    },
    { label: 'WebSocket 连接', value: durationValue(latencyDetails?.wsConnectMs) },
    { label: '上游响应头', value: durationValue(latencyDetails?.upstreamHeadersMs) },
    { label: '首个上游事件', value: durationValue(latencyDetails?.firstEventMs) },
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

  const upstreamDisplay = formatDuration(upstreamMs)
  const upstreamItems: UsageTimingItem[] = []
  if (upstreamMs !== null)
    upstreamItems.push({ label: '响应耗时', value: upstreamDisplay })
  const processingMs = durationValue(latencyDetails?.openaiProcessingMs)
  if (processingMs !== null)
    upstreamItems.push({ label: '处理耗时', value: formatDuration(processingMs) })

  const performanceItems = [
    { label: 'API 开销', value: durationValue(latencyDetails?.upstreamApiOverheadMs) },
    { label: '引擎耗时', value: durationValue(latencyDetails?.upstreamEngineMs) },
    { label: 'TTFT · IAPI', value: durationValue(latencyDetails?.upstreamEngineIapiTtftMs) },
    { label: 'TTFT · Service', value: durationValue(latencyDetails?.upstreamEngineServiceTtftMs) },
    { label: 'Token 间隔 · IAPI', value: durationValue(latencyDetails?.upstreamEngineIapiTbtMs) },
    { label: 'Token 间隔 · Service', value: durationValue(latencyDetails?.upstreamEngineServiceTbtMs) },
  ]
  for (const item of performanceItems) {
    if (item.value !== null)
      upstreamItems.push({ label: item.label, value: formatUpstreamDuration(item.value) })
  }

  const sections: UsageTimingSection[] = []
  if (upstreamItems.length) {
    sections.push({
      title: '上游性能',
      source: 'official',
      items: upstreamItems,
    })
  }
  sections.push({ title: '请求观测', source: 'local', items: requestItems })
  if (transportItems.length) {
    sections.push({
      title: '传输观测',
      source: 'local',
      items: transportItems,
    })
  }

  return {
    upstreamDisplay,
    firstOutputDisplay: formatDuration(firstTokenMs),
    totalDisplay: formatDuration(totalMs),
    sections,
  }
}

export function usageBillingText(record: Pick<UsageCommonRecord, 'billing'>) {
  return record.billing?.totalAmountDisplay || '—'
}

function durationValue(value: unknown) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null
}

function formatUpstreamDuration(value: number) {
  if (value >= 1000)
    return formatDuration(value)
  if (value > 0 && value < 0.001)
    return '<0.001 ms'
  return `${Number(value.toFixed(3))} ms`
}

function upstreamResponseMs(details: UsageCommonRecord['latencyDetails']) {
  const value = durationValue(details?.upstreamResponseMs)
  return value !== null && value > 0 ? value : null
}
