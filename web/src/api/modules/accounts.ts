import { requestJson, requestPageJson } from '../request'
import type { PaginatedResult } from '../types'

export interface Account {
  id: string
  email?: string
  accountId?: string
  userId?: string
  planType?: string
  status: 'active' | 'expired' | 'disabled' | 'banned' | 'quota_exhausted' | 'refreshing'
  label?: string
  accessTokenExpiresAt?: string
  accessTokenExpiresAtDisplay?: string
  addedAt: string
  addedAtDisplay: string
  updatedAt: string
  updatedAtDisplay: string
  quota: AccountQuota
  usage: AccountUsage
}

export interface AccountQuota {
  refreshedAtDisplay: string
  windows: AccountQuotaWindow[]
}

export interface AccountQuotaWindow {
  key: string
  group: 'monthly' | 'shortTerm' | 'other'
  windowSeconds?: number | null
  labelDisplay: string
  usedPercent?: number | null
  usedPercentDisplay: string
  resetAtDisplay: string
  windowUsedDisplay: string
}

export interface AccountUsage {
  requestCount: number
  requestCountDisplay: string
  emptyResponseCount: number
  inputTokens: number
  inputTokensDisplay: string
  outputTokens: number
  outputTokensDisplay: string
  cachedTokens: number
  cachedTokensDisplay: string
  reasoningTokens: number
  totalTokens: number
  totalTokensDisplay: string
  imageInputTokens: number
  imageOutputTokens: number
  imageTokensDisplay: string
  imageRequestCount: number
  imageRequestFailedCount: number
  createdTokens: number
  createdTokensDisplay: string
  readTokens: number
  readTokensDisplay: string
  lastUsedAt?: string
  lastUsedAtDisplay: string
  models: AccountModelUsage[]
}

export interface AccountModelUsage {
  model: string
  requestCount: number
  requestCountDisplay: string
  successRate: number
  successRateDisplay: string
  inputTokens: number
  inputTokensDisplay: string
  outputTokens: number
  outputTokensDisplay: string
  cachedTokens: number
  cachedTokensDisplay: string
  totalTokens: number
  totalTokensDisplay: string
  totalCostUsd: number
  totalCostUsdDisplay: string
  lastUsedAt?: string
  lastUsedAtDisplay: string
}

export interface AccountsQuery {
  page: number
  pageSize: number
  search?: string
}

export function getAccounts(): Promise<Account[]>
export function getAccounts(query: AccountsQuery): Promise<PaginatedResult<Account>>
export async function getAccounts(query?: AccountsQuery) {
  if (query) {
    return requestPageJson<Account>('/api/admin/accounts', {
      method: 'GET',
      params: compactQuery(query),
    })
  }

  const result = await requestPageJson<Account>('/api/admin/accounts')
  return result.items
}

function compactQuery(query: AccountsQuery) {
  return {
    page: query.page,
    pageSize: query.pageSize,
    search: query.search?.trim() || undefined,
  }
}

export interface CreateAccountPayload {
  refreshToken: string
  label?: string
}

export function createAccount(payload: CreateAccountPayload) {
  return requestJson<Account>('/api/admin/accounts', {
    method: 'POST',
    data: payload,
  })
}

export function deleteAccount(accountId: string) {
  return requestJson<{ deleted: number }>('/api/admin/accounts/delete', {
    method: 'POST',
    data: { ids: [accountId] },
  })
}

export function refreshAccount(accountId: string) {
  return requestJson<Account>('/api/admin/accounts/refresh', {
    method: 'POST',
    data: { id: accountId },
  })
}

export interface UpdateAccountPayload {
  label?: string | null
  email?: string | null
  accountId?: string | null
  userId?: string | null
  planType?: string | null
  status?: Account['status']
}

export function updateAccount(accountId: string, payload: UpdateAccountPayload) {
  return requestJson<Account>('/api/admin/accounts/update', {
    method: 'POST',
    data: { id: accountId, ...payload },
  })
}

export function updateAccountLabel(accountId: string, label: string | null) {
  return updateAccount(accountId, { label })
}

export function updateAccountStatus(accountId: string, status: Account['status']) {
  return updateAccount(accountId, { status })
}

export function batchDeleteAccounts(accountIds: string[]) {
  return requestJson<{ deleted: number }>('/api/admin/accounts/delete', {
    method: 'POST',
    data: { ids: accountIds },
  })
}

export interface QuotaInfo {
  requestsCount: number
  requestsLimit: number
  resetAt: string
}

export function getAccountQuota(accountId: string) {
  const params = new URLSearchParams({ id: accountId })
  return requestJson<QuotaInfo>(`/api/admin/accounts/quota?${params}`)
}

export interface AccountHealthCheckSummary {
  total: number
  alive: number
  dead: number
  skipped: number
}

export interface AccountHealthCheckResult {
  id: string
  email?: string | null
  result: 'alive' | 'dead' | string
  error?: string
  durationMs?: number
}

export interface AccountHealthCheckData {
  summary: AccountHealthCheckSummary
  results: AccountHealthCheckResult[]
}

export function testAccountConnection(accountId: string) {
  return requestJson<AccountHealthCheckData>('/api/admin/accounts/health-check', {
    method: 'POST',
    data: { ids: [accountId] },
  })
}
