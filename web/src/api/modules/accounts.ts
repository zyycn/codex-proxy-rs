import { requestJson } from '../request'

export interface Account {
  id: string
  email: string
  accountId?: string
  userId?: string
  planType?: string
  status: 'active' | 'expired' | 'disabled' | 'banned' | 'quota_exhausted' | 'refreshing'
  label?: string
  accessTokenExpiresAt?: string
  nextRefreshAt?: string
  createdAt: string
  lastUsedAt?: string
  totalRequests: number
  totalInputTokens: number
  totalOutputTokens: number
}

export function getAccounts() {
  return requestJson<Account[]>('/api/admin/accounts')
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
  return requestJson<void>(`/api/admin/accounts/${accountId}`, {
    method: 'DELETE',
  })
}

export function refreshAccount(accountId: string) {
  return requestJson<Account>(`/api/admin/accounts/${accountId}/refresh`, {
    method: 'POST',
  })
}

export function updateAccountLabel(accountId: string, label: string | null) {
  return requestJson<Account>(`/api/admin/accounts/${accountId}/label`, {
    method: 'PATCH',
    data: { label },
  })
}

export function updateAccountStatus(accountId: string, status: Account['status']) {
  return requestJson<Account>(`/api/admin/accounts/${accountId}/status`, {
    method: 'PATCH',
    data: { status },
  })
}

export function batchDeleteAccounts(accountIds: string[]) {
  return requestJson<{ deleted: number }>('/api/admin/accounts/batch-delete', {
    method: 'POST',
    data: { accountIds },
  })
}

export interface QuotaInfo {
  requestsCount: number
  requestsLimit: number
  resetAt: string
}

export function getAccountQuota(accountId: string) {
  return requestJson<QuotaInfo>(`/api/admin/accounts/${accountId}/quota`)
}
