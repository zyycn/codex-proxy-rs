import type { UsageListRecord, UsageRecordDetail, UsageTokenDetails } from '@/api'
import { t } from '@/i18n'
import { formatCompactNumber, formatDuration, formatInteger } from '@/utils/format'

// 列表与详情使用独立读模型，展示函数只依赖两者的公共字段。
type UsageCommonRecord = UsageListRecord | UsageRecordDetail
type UsageLatencyRecord = Pick<UsageCommonRecord, 'latencyDetails' | 'firstTokenLatencyMs' | 'latencyMs'>

export type UsagePerformanceRecord = Pick<UsageCommonRecord, 'latencyDetails' | 'firstTokenLatencyMs'> & {
  tokenDetails: Pick<UsageListRecord['tokenDetails'], 'outputTokens'> | null
}

export type UsageModelRecord = Pick<UsageCommonRecord, 'model'> & Partial<Pick<UsageCommonRecord, 'requestedModel' | 'upstreamModel'>>
export type UsageReasoningRecord = Pick<UsageCommonRecord, 'reasoningEffort'> & Partial<Pick<UsageCommonRecord, 'reasoningPreset' | 'subagentKind'>>

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
    inputTokensDisplay: inputTokens === null
      ? details.inputTokensDisplay
      : formatInteger(inputTokens),
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

export function usageAccountText(record: Pick<UsageCommonRecord, 'accountEmail' | 'accountName' | 'accountId'>) {
  return record.accountEmail || record.accountName || record.accountId || '—'
}

export function usageClientIp(record: { clientIp?: string | null }) {
  return record.clientIp || '—'
}

export function usageUserAgent(record: { userAgent?: string | null }) {
  return record.userAgent || '—'
}

export function usageReasoningEffort(record: UsageReasoningRecord) {
  const reasoningEffort = record.reasoningEffort || '—'
  if (record.subagentKind)
    return reasoningEffort
  return record.reasoningPreset || reasoningEffort
}

export function usageModelDisplay(record: UsageModelRecord) {
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

export function usagePerformanceDetails(record: UsagePerformanceRecord) {
  const upstreamMs = upstreamResponseMs(record.latencyDetails)
  const outputTokens = record.tokenDetails?.outputTokens
  // 输出已包含推理 Token，分母只取同一完成响应的官方时间跨度
  const throughput = typeof outputTokens === 'number' && Number.isFinite(outputTokens) && outputTokens > 0
    && upstreamMs !== null
    ? outputTokens * 1000 / upstreamMs
    : null

  return {
    throughputDisplay: throughput === null
      ? '—'
      : `${formatCompactNumber(throughput)} tok/s`,
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
    requestItems.push({
      label: t('usage.status.firstOutputWaiting'),
      value: formatDuration(firstTokenMs),
    })

    if (firstTextMs !== null && firstTextMs >= firstTokenMs && firstTextMs <= totalMs) {
      const beforeTextMs = firstTextMs - firstTokenMs
      if (beforeTextMs > 0) {
        requestItems.push({
          label: firstReasoningMs === firstTokenMs
            ? t('usage.status.reasoningToText')
            : t('usage.status.firstOneIsOutputToText'),
          value: formatDuration(beforeTextMs),
        })
      }
      requestItems.push({
        label: t('usage.status.textToCompletion'),
        value: formatDuration(totalMs - firstTextMs),
      })
    }
    else {
      requestItems.push({
        label: t('usage.status.completedAfterFirstOutput'),
        value: formatDuration(totalMs - firstTokenMs),
      })
    }
  }

  requestItems.push({
    label: t('usage.latency.totalLatency'),
    value: formatDuration(totalMs),
    emphasized: true,
  })

  const transportItems: UsageTimingItem[] = [
    {
      label: t('usage.status.admissionDecision'),
      value: durationValue(latencyDetails?.admissionDecisionMs),
    },
    {
      label: t('usage.status.accountSelectionWaiting'),
      value: durationValue(latencyDetails?.accountSelectionWaitMs),
    },
    {
      label: t('usage.status.transferDecisionWaiting'),
      value: durationValue(latencyDetails?.transportDecisionWaitMs),
    },
    {
      label: t('usage.status.websocketConnection'),
      value: durationValue(latencyDetails?.wsConnectMs),
    },
    {
      label: t('usage.status.upstreamResponseHeader'),
      value: durationValue(latencyDetails?.upstreamHeadersMs),
    },
    {
      label: t('usage.status.firstUpstreamEvent'),
      value: durationValue(latencyDetails?.firstEventMs),
    },
  ]
    .filter(item => item.value !== null)
    .map(item => ({
      ...item,
      value: formatDuration(item.value),
    }))

  if (
    latencyDetails?.capacityUsedSlots != null
    && latencyDetails.capacityTotalSlots != null
  ) {
    transportItems.push({
      label: t('usage.status.accountSlotSnapshot'),
      value: `${latencyDetails.capacityUsedSlots} / ${latencyDetails.capacityTotalSlots}`,
    })
  }

  const upstreamDisplay = formatDuration(upstreamMs)
  const upstreamItems: UsageTimingItem[] = []
  if (upstreamMs !== null) {
    upstreamItems.push({
      label: t('accounts.connectionTest.duration'),
      value: upstreamDisplay,
    })
  }
  const processingMs = durationValue(latencyDetails?.openaiProcessingMs)
  if (processingMs !== null) {
    upstreamItems.push({
      label: t('usage.status.processingTime'),
      value: formatDuration(processingMs),
    })
  }

  const performanceItems = [
    {
      label: t('usage.status.apiOverhead'),
      value: durationValue(latencyDetails?.upstreamApiOverheadMs),
    },
    {
      label: t('usage.status.engineTime'),
      value: durationValue(latencyDetails?.upstreamEngineMs),
    },
  ]
  for (const item of performanceItems) {
    if (item.value !== null) {
      upstreamItems.push({
        label: item.label,
        value: formatUpstreamDuration(item.value),
      })
    }
  }

  const sections: UsageTimingSection[] = []
  if (upstreamItems.length) {
    sections.push({
      title: t('usage.status.upstreamPerformance'),
      source: 'official',
      items: upstreamItems,
    })
  }
  sections.push({
    title: t('usage.status.requestObservation'),
    source: 'local',
    items: requestItems,
  })
  if (transportItems.length) {
    sections.push({
      title: t('usage.status.transport'),
      source: 'local',
      items: transportItems,
    })
  }

  return {
    upstreamDisplay,
    firstOutputDisplay: formatDuration(firstTokenMs),
    firstOutputSeverity: latencySeverity(firstTokenMs, [10_000, 30_000, 60_000]),
    totalDisplay: formatDuration(totalMs),
    totalSeverity: latencySeverity(totalMs, [60_000, 180_000, 300_000]),
    sections,
  }
}

function latencySeverity(ms: number | null, thresholds: readonly [number, number, number]): 'good' | 'warn' | 'slow' | 'critical' | null {
  if (ms === null)
    return null
  // 耗时按固定阈值分档，边界值进入下一档，不改变请求状态
  if (ms >= thresholds[2])
    return 'critical'
  if (ms >= thresholds[1])
    return 'slow'
  if (ms >= thresholds[0])
    return 'warn'
  return 'good'
}

export function usageBillingText(record: Pick<UsageCommonRecord, 'billing'>) {
  return record.billing?.totalAmountDisplay || '—'
}

function durationValue(value: unknown) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0
    ? value
    : null
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
  return value !== null && value > 0
    ? value
    : null
}
