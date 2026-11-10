import type { RequestOptions } from '../../request'
import type { Account, AccountIdParam, AccountModelAccess, AccountRefreshResponse } from './types'
import request from '../../request'

export interface AccountPageMeta {
  page: number
  pageSize: number
  total: number
  totalPages: number
}

export interface AccountSummary {
  total: number
  normal: number
  quotaPaused: number
  quotaExhausted: number
  rateLimited: number
  disabled: number
  error: number
}

export interface AccountListResponse {
  items: Account[]
  page: AccountPageMeta
  summary: AccountSummary
}

export interface AccountUpdateResponse {
  accountId: string
  configRevision: number
}

export interface AccountBatchUpdateResponse {
  accountIds: string[]
  configRevision: number
}

export interface AccountDeletionResponse {
  deletedCount: number
  accountIds: string[]
}

// 请求参数类型：仅定义 API 边界的形状，调用方不依赖显式声明。
interface AccountListParams {
  page: number
  pageSize: number
  search?: string
  provider?: string
  status?: string
  groupId?: string
  sortBy?: string
  sortDirection?: string
}

interface AccountUpdateParam {
  connection?: { baseUrl?: string, protocol?: ApiKeyConfiguration['protocol'], transport: ApiKeyConfiguration['transport'], apiKey?: string }
  outboundProxyUrl?: string
  outboundProxyId?: string
  accountId: string
  name?: string
  notes?: string
  enabled: boolean
  concurrencyLimit: number | null
  weight: number
  modelAccess?: AccountModelAccess
  groupIds: string[]
}

interface AccountBatchUpdateParam {
  quotaLimits?: Record<string, number>
  autoUseQuotaCredits?: boolean
  outboundProxyUrl?: string
  outboundProxyId?: string
  accountIds: string[]
  enabled?: boolean
  concurrencyLimit?: number | null
  weight?: number
  modelAccess?: AccountModelAccess
  groupIds?: string[]
}

interface AccountDeleteParams {
  provider: string
  accountIds: string[]
}

interface AccountDetailResponse {
  account: Account
  credentialConfiguration?: Record<string, unknown>
}

interface AccountConcurrencyParams extends AccountIdParam {
  page: number
  pageSize: number
}

export interface ApiKeyConfiguration {
  base_url: string
  protocol: 'responses' | 'messages'
  transport: 'http' | 'prefer_websocket'
}

export interface AccountConcurrencyEntry {
  leaseId: string
  requestId: string | null
  clientKeyId: string | null
  clientKeyName: string | null
  model: string | null
  clientIp: string | null
  startedAt: string | null
  startedAtDisplay: string | null
  occupiedMs: number | null
}

export interface AccountConcurrencyPage {
  accountId: string
  entries: AccountConcurrencyEntry[]
  total: number
  page: number
  pageSize: number
  observedAt: string
}

export function getAccounts(data: AccountListParams, options: RequestOptions = {}) {
  return request<AccountListResponse>({
    url: '/api/admin/accounts',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getAccountDetail(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountDetailResponse>({
    url: '/api/admin/accounts/detail',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function recoverAccount(data: AccountIdParam) {
  return request<AccountRefreshResponse>({
    url: '/api/admin/accounts/recover',
    method: 'POST',
    data,
  })
}

export function updateAccount(data: AccountUpdateParam) {
  return request<AccountUpdateResponse>({
    url: '/api/admin/accounts/update',
    method: 'POST',
    data,
  })
}

export function batchUpdateAccounts(data: AccountBatchUpdateParam) {
  return request<AccountBatchUpdateResponse>({
    url: '/api/admin/accounts/batch-update',
    method: 'POST',
    data,
  })
}

export function deleteAccounts(data: AccountDeleteParams, options: RequestOptions = {}) {
  return request<AccountDeletionResponse>({
    url: '/api/admin/accounts/delete',
    method: 'POST',
    data,
    ...options,
  })
}

export function getAccountConcurrency(data: AccountConcurrencyParams, options: RequestOptions = {}) {
  return request<AccountConcurrencyPage>({
    url: '/api/admin/accounts/concurrency',
    method: 'GET',
    params: data,
    ...options,
  })
}
