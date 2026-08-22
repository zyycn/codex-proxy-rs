import type { AccountErrorReason, AccountStatus, getAccounts } from '@/api'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import { formatProviderLabel } from '@/utils/providers'

export type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]
export type AccountQuotaWindow = AccountRow['quota']['windows'][number]

export interface AccountQuotaWindowEntry {
  key: string
  label: string | null
  windows: AccountQuotaWindow[]
}

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

export function groupedAccountQuotaWindows(windows: AccountQuotaWindow[]) {
  const entries: AccountQuotaWindowEntry[] = []
  const limitEntryIndexes = new Map<string, number>()

  for (const window of windows) {
    const limitLabel = quotaLimitLabel(window)
    if (!window.limitId || !limitLabel) {
      entries.push({ key: window.key, label: null, windows: [window] })
      continue
    }

    const existingIndex = limitEntryIndexes.get(window.limitId)
    if (existingIndex !== undefined) {
      entries[existingIndex]?.windows.push(window)
      continue
    }

    limitEntryIndexes.set(window.limitId, entries.length)
    entries.push({
      key: `limit:${window.limitId}`,
      label: limitLabel,
      windows: [window],
    })
  }

  for (const entry of entries)
    entry.windows.sort(compareQuotaLimitWindows)

  return entries
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

  // 同组保留 Provider 的投影顺序，避免按窗口时长打散 core 与模型专属额度。
  return 0
}

function groupOrder(window: AccountQuotaWindow) {
  return quotaGroupOrder.get(window.group) ?? quotaGroupOrder.size
}

function compareQuotaLimitWindows(left: AccountQuotaWindow, right: AccountQuotaWindow) {
  const durationDifference = windowDurationOrder(left.windowSeconds)
    - windowDurationOrder(right.windowSeconds)
  if (durationDifference !== 0)
    return durationDifference

  return windowRoleOrder(left.role) - windowRoleOrder(right.role)
}

function windowDurationOrder(windowSeconds: number | null) {
  return windowSeconds ?? Number.POSITIVE_INFINITY
}

function windowRoleOrder(role: AccountQuotaWindow['role']) {
  switch (role) {
    case 'primary':
      return 0
    case 'secondary':
      return 1
    case 'monthly':
      return 2
    default:
      return 3
  }
}

function quotaLimitLabel(window: AccountQuotaWindow) {
  if (window.limitId === 'codex')
    return '通用额度'
  return window.limitName
}

function accountProviderLabel(value?: string | null) {
  return formatProviderLabel(value)
}

function optionalAccountCell(value: unknown) {
  return value === '—' || value === '-' ? '' : value
}
