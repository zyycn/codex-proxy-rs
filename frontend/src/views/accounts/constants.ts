import type { Account, AccountErrorReason, AccountQuotaWindow, AccountStatus, AccountUsage } from '@/api'
import { defineTableColumns } from '@codex-proxy/ui'
import { computed } from 'vue'
import { t } from '@/i18n'
import { formatProviderLabel } from '@/utils/providers'

export interface AccountQuotaWindowEntry {
  key: string
  label: string | null
  windows: AccountQuotaWindow[]
}

export const usageWindowLabels = computed<Record<AccountUsage['windowKind'], string>>(() => ({
  general: t('accounts.usage.windows.general'),
  weekly: t('accounts.usage.windows.weekly'),
  monthly: t('accounts.usage.windows.monthly'),
  quota: t('accounts.usage.windows.quota'),
}))

const quotaGroupOrder = new Map([
  ['shortTerm', 0],
  ['monthly', 1],
  ['other', 2],
])

export const accountColumns = computed(() => (defineTableColumns<Account>([
  { key: 'expander', kind: 'expander', hideable: false },
  { key: 'selection', kind: 'selection', hideable: false },
  {
    key: 'identity',
    hideable: false,
    label: t('accounts.account'),
    kind: 'identity',
    size: '3xl',
    sortable: 'email',
  },
  {
    key: 'provider',
    label: t('accounts.fields.platformType'),
    kind: 'meta',
    size: 'md',
    align: 'center',
    formatter: value => formatProviderLabel(typeof value === 'string'
      ? value
      : null),
  },
  {
    key: 'status',
    label: t('common.fields.status'),
    kind: 'status',
    align: 'center',
    sortable: true,
  },
  {
    key: 'planType',
    label: t('accounts.profile.subscription.label'),
    kind: 'status',
    size: 'sm',
    sortable: true,
  },
  {
    key: 'usage',
    label: t('usage.usage'),
    kind: 'custom',
    size: '2xl',
    sortable: true,
  },
  {
    key: 'groups',
    label: t('accounts.fields.accountGrouping'),
    kind: 'status',
    size: 'sm',
  },
  {
    key: 'lastUsedAt',
    label: t('accounts.fields.lastUsed'),
    kind: 'datetime',
    size: 'md',
    sortable: true,
    emptyText: '',
  },
  {
    key: 'accessTokenExpiresAtDisplay',
    label: t('accounts.fields.tokenExpires'),
    kind: 'datetime',
    sortable: 'expiresAt',
    formatter: value => optionalAccountCell(value),
    emptyText: '',
  },
  {
    key: 'actions',
    label: t('common.fields.actions'),
    kind: 'actions',
    size: 'lg',
    hideable: false,
  },
])))

export const statusLabels = computed<Record<AccountStatus, string>>(() => ({
  normal: t('common.status.healthy'),
  quota_exhausted: t('accounts.fields.quotaExhausted'),
  rate_limited: t('accounts.fields.rateLimiting'),
  disabled: t('accounts.fields.deactivated'),
  error: t('accounts.fields.error'),
}))

export const statusTones: Record<AccountStatus, 'success' | 'danger' | 'warning' | 'info' | 'normal'> = {
  normal: 'success',
  quota_exhausted: 'warning',
  rate_limited: 'warning',
  disabled: 'normal',
  error: 'danger',
}

export const accountStatusFilterOptions = computed(() => ([
  {
    label: t('common.status.all'),
    value: '',
  },
  { label: statusLabels.value.normal, value: 'normal' },
  { label: statusLabels.value.quota_exhausted, value: 'quota_exhausted' },
  { label: statusLabels.value.rate_limited, value: 'rate_limited' },
  { label: statusLabels.value.disabled, value: 'disabled' },
  { label: statusLabels.value.error, value: 'error' },
]))

/** `error` 分类下具体原因的展示文案（对应后端 `errorReason`）。 */
export const errorReasonLabels = computed<Record<AccountErrorReason, string>>(() => ({
  account_unverified: t('accounts.fields.identityUnconfirmed'),
  access_token_expired: t('accounts.fields.accessTokenHasExpired'),
  credential_expired: t('accounts.fields.credentialsHaveExpired'),
  credential_invalid: t('accounts.fields.invalidCredentials'),
  account_banned: t('accounts.fields.accountUnavailable'),
}))

export function visibleSummaryQuotaWindows(windows: AccountQuotaWindow[]) {
  const known = [...windows]
    .filter(window => window.group !== 'other')
    .sort(compareQuotaWindows)
  return known.length > 0
    ? known
    : [...windows].sort(compareQuotaWindows)
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
      entries.push({
        key: window.key,
        label: null,
        windows: [window],
      })
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
    return 'text-cp-text-quaternary'
  if (successRate >= 99.5)
    return 'text-cp-success-text'
  if (successRate >= 98)
    return 'text-cp-cyan-text'
  if (successRate >= 95)
    return 'text-cp-warning-text'
  return 'text-cp-error-text'
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
    return t('accounts.quota.general')
  return window.limitName
}

function optionalAccountCell(value: unknown) {
  return value === '—' || value === '-'
    ? ''
    : value
}
