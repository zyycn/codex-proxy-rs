import type { MetricTone } from './composables/useDashboard'
import { formatInteger } from '@/utils/number'

// tone 到 Tailwind 类的唯一映射；仪表盘各卡片共用。
export const metricToneIconClasses: Record<MetricTone, string> = {
  normal: 'bg-cp-status-normal-bg text-cp-status-normal',
  info: 'bg-cp-info-bg text-cp-info',
  success: 'bg-cp-success-bg text-cp-success',
  warning: 'bg-cp-warning-bg text-cp-warning',
  danger: 'bg-cp-error-bg text-cp-error',
}

export const metricToneValueClasses: Record<MetricTone, string> = {
  normal: 'text-cp-status-normal-text',
  info: 'text-cp-info-text',
  success: 'text-cp-success-text',
  warning: 'text-cp-warning-text',
  danger: 'text-cp-error-text',
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
    cellClass: 'bg-cp-bg-container-disabled opacity-60',
    badgeClass: 'bg-cp-fill-tertiary text-cp-text-quaternary',
  },
  no_data: {
    label: '无有效样本',
    cellClass: 'bg-cp-border',
    badgeClass: 'bg-cp-fill-tertiary text-cp-text-secondary',
  },
  unavailable: {
    label: '不可达',
    cellClass: 'bg-cp-error',
    badgeClass: 'bg-cp-error-bg text-cp-error-text',
  },
  unstable: {
    label: '不稳定',
    cellClass: 'bg-cp-warning',
    badgeClass: 'bg-cp-warning-bg text-cp-warning-text',
  },
  low_sample: {
    label: '低样本',
    cellClass: 'bg-cp-status-normal',
    badgeClass: 'bg-cp-status-normal-bg text-cp-status-normal-text',
  },
  stable: {
    label: '稳定',
    cellClass: 'bg-cp-success',
    badgeClass: 'bg-cp-success-bg text-cp-success-text',
  },
}

export function healthReliabilityValueClass(successRequests: number, failedRequests: number) {
  const eligibleRequests = Math.max(0, successRequests) + Math.max(0, failedRequests)
  if (eligibleRequests === 0)
    return 'text-cp-text-quaternary'

  const reliability = (Math.max(0, successRequests) / eligibleRequests) * 100
  if (reliability >= 99.5)
    return 'text-cp-success-text'
  if (reliability >= 98)
    return 'text-cp-status-normal-text'
  if (reliability >= 95)
    return 'text-cp-warning-text'
  return 'text-cp-error-text'
}

export function formatHealthCount(value: number) {
  return formatInteger(Math.max(0, value))
}
