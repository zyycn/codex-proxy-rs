import type { MetricTone } from './composables/presenter'

// tone 到 Tailwind 类的唯一映射；仪表盘各卡片共用。
export const metricToneIconClasses: Record<MetricTone, string> = {
  normal: 'bg-(--cp-normal-bg) text-(--cp-normal)',
  info: 'bg-(--cp-info-bg) text-(--cp-info)',
  success: 'bg-(--cp-success-bg) text-(--cp-success)',
  warning: 'bg-(--cp-warning-bg) text-(--cp-warning)',
  danger: 'bg-(--cp-danger-bg) text-(--cp-danger)',
}

export const metricToneValueClasses: Record<MetricTone, string> = {
  normal: 'text-(--cp-normal-text)',
  info: 'text-(--cp-info-text)',
  success: 'text-(--cp-success-text)',
  warning: 'text-(--cp-warning-text)',
  danger: 'text-(--cp-danger-text)',
}

export type HealthStatus
  = 'future' | 'no_data' | 'unavailable' | 'unstable' | 'low_sample' | 'stable'

export interface HealthTimelinePoint {
  time: string
  status: HealthStatus
  reliabilityDisplay: string
  successRequests: number
  failedRequests: number
  cancelledRequests: number
  callerErrorRequests: number
}

export interface HealthTimeline {
  title: string
  description: string
  reliabilityDisplay: string
  status: HealthStatus
  successRequests: number
  failedRequests: number
  cancelledRequests: number
  callerErrorRequests: number
  points: HealthTimelinePoint[]
}

interface HealthStatusMeta {
  label: string
  cellClass: string
  badgeClass: string
}

export const healthLegend = [
  { status: 'no_data', label: '无有效样本' },
  { status: 'unavailable', label: '不可达' },
  { status: 'unstable', label: '不稳定' },
  { status: 'low_sample', label: '低样本' },
  { status: 'stable', label: '稳定' },
] satisfies { status: HealthStatus, label: string }[]

export const healthStatusMeta: Record<HealthStatus, HealthStatusMeta> = {
  future: {
    label: '未来',
    cellClass: 'bg-(--cp-disabled-bg) opacity-60',
    badgeClass: 'bg-(--cp-bg-muted) text-(--cp-text-muted)',
  },
  no_data: {
    label: '无有效样本',
    cellClass: 'bg-(--cp-default-border-hover)',
    badgeClass: 'bg-(--cp-bg-muted) text-(--cp-text-secondary)',
  },
  unavailable: {
    label: '不可达',
    cellClass: 'bg-(--cp-danger)',
    badgeClass: 'bg-(--cp-danger-bg) text-(--cp-danger-text)',
  },
  unstable: {
    label: '不稳定',
    cellClass: 'bg-(--cp-warning)',
    badgeClass: 'bg-(--cp-warning-bg) text-(--cp-warning-text)',
  },
  low_sample: {
    label: '低样本',
    cellClass: 'bg-(--cp-normal)',
    badgeClass: 'bg-(--cp-normal-bg) text-(--cp-normal-text)',
  },
  stable: {
    label: '稳定',
    cellClass: 'bg-(--cp-success)',
    badgeClass: 'bg-(--cp-success-bg) text-(--cp-success-text)',
  },
}

export function healthReliabilityValueClass(successRequests: number, failedRequests: number) {
  const eligibleRequests = Math.max(0, successRequests) + Math.max(0, failedRequests)
  if (eligibleRequests === 0)
    return 'text-(--cp-text-muted)'

  const reliability = (Math.max(0, successRequests) / eligibleRequests) * 100
  if (reliability >= 99.5)
    return 'text-(--cp-success-text)'
  if (reliability >= 98)
    return 'text-(--cp-normal-text)'
  if (reliability >= 95)
    return 'text-(--cp-warning-text)'
  return 'text-(--cp-danger-text)'
}

export function formatHealthCount(value: number) {
  return Math.max(0, value).toLocaleString('zh-CN')
}
