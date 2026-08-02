import type { getAccounts } from '@/api'
import type { BaseTableColumn } from '@/components/base/BaseTable/columns'
import { clamp } from 'es-toolkit'
import { providerDisplayName } from '@/utils/providers'

export type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]
export type AccountQuotaWindow = AccountRow['quota']['windows'][number]

export interface AccountRequestBucket {
  bucketStart: string
  requestCount: number
}

export interface AccountLocalUsage {
  requestCount?: number
  requestCountDisplay?: string
  totalTokens?: number
  totalTokensDisplay?: string
  requestBuckets?: AccountRequestBucket[]
}

const summaryGroupOrder = new Map([
  ['shortTerm', 0],
  ['monthly', 1],
])

const panelGroupOrder = new Map([
  ['monthly', 0],
  ['shortTerm', 1],
  ['other', 2],
])

const relaxedCellClass = 'py-3 align-middle'

export const accountColumns = [
  {
    key: 'expander',
    label: '',
    width: '40px',
    minWidth: '40px',
    align: 'center' as const,
    headerClass: '!px-2',
    cellClass: `!px-2 ${relaxedCellClass}`,
  },
  {
    key: 'selection',
    label: '',
    width: '40px',
    minWidth: '40px',
    align: 'center' as const,
    headerClass: '!px-2',
    cellClass: `!px-2 ${relaxedCellClass}`,
  },
  {
    key: 'identity',
    label: '邮箱',
    sortable: true,
    sortKey: 'email',
    width: '270px',
    minWidth: '270px',
    cellClass: relaxedCellClass,
  },
  {
    key: 'provider',
    label: '平台/类型',
    width: '120px',
    minWidth: '120px',
    align: 'center' as const,
    format: value => accountProviderLabel(typeof value === 'string' ? value : null),
    cellClass: `${relaxedCellClass} text-(--cp-text-secondary)`,
  },
  {
    key: 'status',
    label: '状态',
    sortable: true,
    flex: 0.6,
    minWidth: '60px',
    cellClass: relaxedCellClass,
  },
  {
    key: 'planType',
    label: '套餐',
    sortable: true,
    flex: 0.8,
    minWidth: '112px',
    cellClass: relaxedCellClass,
  },
  {
    key: 'usage',
    label: '用量',
    sortable: true,
    flex: 1.3,
    minWidth: '220px',
    cellClass: relaxedCellClass,
  },
  {
    key: 'lastUsedAt',
    label: '最后使用',
    sortable: true,
    flex: 1.2,
    minWidth: '160px',
    format: (_value: unknown, row: AccountRow) => optionalAccountCell(row.usage.lastUsedAtDisplay),
    emptyText: '',
    cellClass: `${relaxedCellClass} text-(--cp-text-secondary)`,
  },
  {
    key: 'accessTokenExpiresAtDisplay',
    label: '过期时间',
    sortable: true,
    sortKey: 'expiresAt',
    flex: 1.2,
    minWidth: '160px',
    format: value => optionalAccountCell(value),
    emptyText: '',
    cellClass: `${relaxedCellClass} text-(--cp-text-secondary)`,
  },
  {
    key: 'actions',
    label: '操作',
    width: '110px',
    minWidth: '110px',
  },
] satisfies BaseTableColumn<AccountRow>[]

export const statusLabels: Record<string, string> = {
  active: '正常',
  expired: '已过期',
  disabled: '已禁用',
  banned: '已封禁',
  quota_exhausted: '配额耗尽',
  rate_limited: '限流中',
}

export const statusTones: Record<string, 'success' | 'danger' | 'warning' | 'info' | 'normal'> = {
  active: 'success',
  expired: 'warning',
  disabled: 'normal',
  banned: 'danger',
  quota_exhausted: 'warning',
  rate_limited: 'warning',
}

export const accountStatusFilterOptions = [
  { label: '全部状态', value: '' },
  { label: statusLabels.active, value: 'active' },
  { label: statusLabels.quota_exhausted, value: 'quota_exhausted' },
  { label: statusLabels.expired, value: 'expired' },
  { label: statusLabels.disabled, value: 'disabled' },
  { label: statusLabels.banned, value: 'banned' },
]

/**
 * 后端已派生互斥状态（含 `rate_limited`，由快照级 quota.limitReached 产生）；
 * 前端只渲染，不再独立派生。
 */
export function derivedAccountStatus(row: AccountRow): string {
  return row.status
}

export function visibleSummaryQuotaWindows(windows: AccountQuotaWindow[]) {
  const known = [...windows]
    .filter(window => summaryGroupOrder.has(window.group))
    .sort((left, right) => groupOrder(left, summaryGroupOrder) - groupOrder(right, summaryGroupOrder))
  return known.length > 0 ? known : windows
}

export function orderedPanelQuotaWindows(windows: AccountQuotaWindow[]) {
  return [...windows].sort(
    (left, right) => groupOrder(left, panelGroupOrder) - groupOrder(right, panelGroupOrder),
  )
}

function quotaWindowPercent(window: AccountQuotaWindow) {
  return clamp(window.usedPercent ?? 0, 0, 100)
}

export function quotaWindowBarStyle(window: AccountQuotaWindow, minimumWidth = '8px') {
  const percent = quotaWindowPercent(window)
  return {
    width: `${percent}%`,
    minWidth: percent > 0 ? minimumWidth : '0',
  }
}

export function quotaWindowBarClass(window: AccountQuotaWindow) {
  if (window.usedPercent === null)
    return 'bg-(--cp-default-border-hover)'
  if (window.usedPercent >= 95)
    return 'bg-(--cp-danger)'
  if (window.usedPercent >= 80)
    return 'bg-(--cp-warning)'
  return 'bg-(--cp-success)'
}

export function quotaWindowPercentTextClass(window: AccountQuotaWindow) {
  if (window.usedPercent === null)
    return 'text-(--cp-text-muted)'
  if (window.usedPercent >= 95)
    return 'text-(--cp-danger-text)'
  if (window.usedPercent >= 80)
    return 'text-(--cp-warning-text)'
  return 'text-(--cp-success-text)'
}

function groupOrder(window: AccountQuotaWindow, order: Map<string, number>) {
  return order.get(window.group) ?? order.size
}

function accountProviderLabel(value?: string | null) {
  return providerDisplayName(value) ?? (value || '—')
}

function optionalAccountCell(value: unknown) {
  return value === '—' || value === '-' ? '' : value
}
