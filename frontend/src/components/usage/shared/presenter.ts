import type { UsageListRecord, UsageRecordDetail } from '@/api'
import { formatDuration } from '@/utils/format'

// 列表与详情使用独立读模型，展示函数只依赖两者的公共字段。
type UsageCommonRecord = UsageListRecord | UsageRecordDetail

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

export function usageLatencyDetails(record: Pick<UsageCommonRecord, 'latencyDetails' | 'firstTokenLatencyMs' | 'latencyMs'>) {
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

export function usageBillingText(record: Pick<UsageCommonRecord, 'billing'>) {
  return record.billing?.totalAmountDisplay || '—'
}

function durationValue(value: unknown) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null
}
