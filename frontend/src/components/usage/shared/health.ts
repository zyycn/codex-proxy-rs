import type { DashboardHealthStatus } from '@/api'
import { formatInteger } from '@/utils/format'

interface DashboardHealthStatusMeta {
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
] satisfies { status: DashboardHealthStatus, label: string }[]

export const healthStatusMeta: Record<DashboardHealthStatus, DashboardHealthStatusMeta> = {
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
    badgeClass: 'bg-cp-error-container text-cp-error-on-container',
  },
  unstable: {
    label: '不稳定',
    cellClass: 'bg-cp-warning',
    badgeClass: 'bg-cp-warning-container text-cp-warning-on-container',
  },
  low_sample: {
    label: '低样本',
    cellClass: 'bg-cp-cyan-solid',
    badgeClass: 'bg-cp-cyan-container text-cp-cyan-on-container',
  },
  stable: {
    label: '稳定',
    cellClass: 'bg-cp-success',
    badgeClass: 'bg-cp-success-container text-cp-success-on-container',
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
    return 'text-cp-cyan-text'
  if (reliability >= 95)
    return 'text-cp-warning-text'
  return 'text-cp-error-text'
}

export function formatHealthCount(value: number) {
  return formatInteger(Math.max(0, value))
}
