import type { DashboardHealthStatus, DashboardHealthTimelinePoint } from '@/api'
import { computed } from 'vue'
import { t } from '@/i18n'
import { formatInteger } from '@/utils/format'

interface DashboardHealthStatusMeta {
  label: string
  cellClass: string
  badgeClass: string
  valueClass: string
}

export const healthLegend = computed(() => ([
  {
    status: 'no_data',
    label: t('usage.noValidSample'),
  },
  {
    status: 'unavailable',
    label: t('usage.notReachable'),
  },
  {
    status: 'unstable',
    label: t('usage.unstable'),
  },
  {
    status: 'low_sample',
    label: t('usage.lowSample'),
  },
  {
    status: 'stable',
    label: t('usage.stable'),
  },
] satisfies { status: DashboardHealthStatus, label: string }[]))

export const healthStatusMeta = computed<Record<DashboardHealthStatus, DashboardHealthStatusMeta>>(() => ({
  future: {
    label: t('usage.future'),
    cellClass: 'bg-cp-bg-container-disabled opacity-60',
    badgeClass: 'bg-cp-fill-tertiary text-cp-text-quaternary',
    valueClass: 'text-cp-text-quaternary',
  },
  no_data: {
    label: t('usage.noValidSample'),
    cellClass: 'bg-cp-border',
    badgeClass: 'bg-cp-fill-tertiary text-cp-text-secondary',
    valueClass: 'text-cp-text-quaternary',
  },
  unavailable: {
    label: t('usage.notReachable'),
    cellClass: 'bg-cp-error',
    badgeClass: 'bg-cp-error-container text-cp-error-on-container',
    valueClass: 'text-cp-error-text',
  },
  unstable: {
    label: t('usage.unstable'),
    cellClass: 'bg-cp-warning',
    badgeClass: 'bg-cp-warning-container text-cp-warning-on-container',
    valueClass: 'text-cp-warning-text',
  },
  low_sample: {
    label: t('usage.lowSample'),
    cellClass: 'bg-cp-cyan-solid',
    badgeClass: 'bg-cp-cyan-container text-cp-cyan-on-container',
    valueClass: 'text-cp-cyan-text',
  },
  stable: {
    label: t('usage.stable'),
    cellClass: 'bg-cp-success',
    badgeClass: 'bg-cp-success-container text-cp-success-on-container',
    valueClass: 'text-cp-success-text',
  },
}))

export function healthServiceFailureCount(point: Pick<DashboardHealthTimelinePoint, 'failedRequests' | 'incompleteRequests'>) {
  return point.failedRequests + point.incompleteRequests
}

export function healthEligibleRequestCount(point: Pick<DashboardHealthTimelinePoint, 'successRequests' | 'failedRequests' | 'incompleteRequests'>) {
  return point.successRequests + healthServiceFailureCount(point)
}

export function healthObservedRequestCount(point: DashboardHealthTimelinePoint) {
  return healthEligibleRequestCount(point) + point.cancelledRequests + point.callerErrorRequests
}

export function formatHealthCount(value: number) {
  return formatInteger(Math.max(0, value))
}
