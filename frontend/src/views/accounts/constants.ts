import type { AccountErrorReason, AccountStatus, getAccounts } from '@/api'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import { formatProviderLabel } from '@/utils/providers'

export type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]
export type AccountQuotaWindow = AccountRow['quota']['windows'][number]

const quotaGroupOrder = new Map([
  ['shortTerm', 0],
  ['monthly', 1],
  ['other', 2],
])

export const accountColumns = defineTableColumns<AccountRow>([
  { key: 'expander', kind: 'expander' },
  { key: 'selection', kind: 'selection' },
  {
    key: 'identity',
    label: '账号',
    kind: 'identity',
    size: '3xl',
    sortable: 'email',
  },
  {
    key: 'provider',
    label: '平台/类型',
    kind: 'meta',
    size: 'md',
    align: 'center',
    format: value => accountProviderLabel(typeof value === 'string' ? value : null),
  },
  { key: 'status', label: '状态', kind: 'status', align: 'left', sortable: true },
  { key: 'planType', label: '套餐', kind: 'status', sortable: true },
  { key: 'usage', label: '用量', kind: 'custom', size: '2xl', sortable: true },
  { key: 'groups', label: '账号分组', kind: 'status' },
  {
    key: 'lastUsedAt',
    label: '最后使用',
    kind: 'datetime',
    sortable: true,
    emptyText: '',
  },
  {
    key: 'accessTokenExpiresAtDisplay',
    label: '过期时间',
    kind: 'datetime',
    sortable: 'expiresAt',
    format: value => optionalAccountCell(value),
    emptyText: '',
  },
  { key: 'actions', label: '操作', kind: 'actions', size: 'lg' },
])

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

export function modelSuccessRateTextClass(successRate: number | null) {
  if (successRate === null)
    return 'text-cp-muted-text'
  if (successRate >= 99.5)
    return 'text-cp-success-text'
  if (successRate >= 98)
    return 'text-cp-normal-text'
  if (successRate >= 95)
    return 'text-cp-warning-text'
  return 'text-cp-danger-text'
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
  return formatProviderLabel(value)
}

function optionalAccountCell(value: unknown) {
  return value === '—' || value === '-' ? '' : value
}
