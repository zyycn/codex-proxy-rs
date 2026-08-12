import type { AccountErrorReason, AccountStatus, getAccounts } from '@/api'
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

const quotaGroupOrder = new Map([
  ['shortTerm', 0],
  ['monthly', 1],
  ['other', 2],
])

const relaxedCellClass = 'py-2 align-middle'

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
    flex: 0.5,
    minWidth: '90px',
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
    headerClass: '!pl-8',
    cellClass: `${relaxedCellClass} !pl-8 text-(--cp-text-secondary)`,
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

export const statusLabels: Record<AccountStatus, string> = {
  normal: '正常',
  quota_exhausted: '配额耗尽',
  rate_limited: '限流中',
  disabled: '已停用',
  error: '错误',
}

export const statusTones: Record<AccountStatus, 'success' | 'danger' | 'warning' | 'info' | 'normal'> = {
  normal: 'success',
  quota_exhausted: 'warning',
  rate_limited: 'warning',
  disabled: 'normal',
  error: 'danger',
}

export const accountStatusFilterOptions = [
  { label: '全部状态', value: '' },
  { label: statusLabels.normal, value: 'normal' },
  { label: statusLabels.quota_exhausted, value: 'quota_exhausted' },
  { label: statusLabels.rate_limited, value: 'rate_limited' },
  { label: statusLabels.disabled, value: 'disabled' },
  { label: statusLabels.error, value: 'error' },
]

/** `error` 分类下具体原因的展示文案（对应后端 `errorReason`）。 */
export const errorReasonLabels: Record<AccountErrorReason, string> = {
  account_unverified: '账号身份尚未确认',
  access_token_expired: 'Access Token 已过期',
  credential_expired: '凭据已过期',
  credential_invalid: '凭据无效',
  account_banned: '账号不可用或已被封禁',
}

/**
 * 后端已派生互斥状态（正常 / 配额耗尽 / 限流中 / 已停用 / 错误）；
 * 前端只渲染，不再独立派生。
 */
export function derivedAccountStatus(row: AccountRow): AccountStatus {
  return row.status
}

export function visibleSummaryQuotaWindows(windows: AccountQuotaWindow[]) {
  const known = [...windows]
    .filter(window => window.group !== 'other')
    .sort(compareQuotaWindows)
  return known.length > 0 ? known : [...windows].sort(compareQuotaWindows)
}

export function orderedPanelQuotaWindows(windows: AccountQuotaWindow[]) {
  return [...windows].sort(compareQuotaWindows)
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

function compareQuotaWindows(left: AccountQuotaWindow, right: AccountQuotaWindow) {
  const groupDifference = groupOrder(left) - groupOrder(right)
  if (groupDifference !== 0)
    return groupDifference

  const secondsDifference = (left.windowSeconds ?? Number.MAX_SAFE_INTEGER)
    - (right.windowSeconds ?? Number.MAX_SAFE_INTEGER)
  if (secondsDifference !== 0)
    return secondsDifference

  // 同组同周期保留 Provider 的投影顺序，避免内部 key 把 additional 排到 core 前面。
  return 0
}

function groupOrder(window: AccountQuotaWindow) {
  return quotaGroupOrder.get(window.group) ?? quotaGroupOrder.size
}

function accountProviderLabel(value?: string | null) {
  return providerDisplayName(value) ?? (value || '—')
}

function optionalAccountCell(value: unknown) {
  return value === '—' || value === '-' ? '' : value
}
