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
    cellClass: 'bg-z-bg-container-disabled opacity-60',
    badgeClass: 'bg-z-fill-tertiary text-z-text-quaternary',
    valueClass: 'text-z-text-quaternary',
  },
  no_data: {
    label: t('usage.noValidSample'),
    cellClass: 'bg-z-border',
    badgeClass: 'bg-z-fill-tertiary text-z-text-secondary',
    valueClass: 'text-z-text-quaternary',
  },
  unavailable: {
    label: t('usage.notReachable'),
    cellClass: 'bg-z-error',
    badgeClass: 'bg-z-error-container text-z-error-on-container',
    valueClass: 'text-z-error-text',
  },
  unstable: {
    label: t('usage.unstable'),
    cellClass: 'bg-z-warning',
    badgeClass: 'bg-z-warning-container text-z-warning-on-container',
    valueClass: 'text-z-warning-text',
  },
  low_sample: {
    label: t('usage.lowSample'),
    cellClass: 'bg-z-cyan-solid',
    badgeClass: 'bg-z-cyan-container text-z-cyan-on-container',
    valueClass: 'text-z-cyan-text',
  },
  stable: {
    label: t('usage.stable'),
    cellClass: 'bg-z-success',
    badgeClass: 'bg-z-success-container text-z-success-on-container',
    valueClass: 'text-z-success-text',
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
