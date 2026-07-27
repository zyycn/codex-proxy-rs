import request from '../request'

export interface AccountQuotaWindow {
  key: string
  group: string
  windowSeconds: number | null
  labelDisplay: string
  usedPercent: number | null
  usedPercentDisplay: string
  localUsage?: unknown
  resetAtDisplay: string
}

export interface AccountQuota {
  refreshedAtDisplay: string
  windows: AccountQuotaWindow[]
}

export interface AccountCurrencyCost {
  currency: string
  estimatedAmount: string
  estimatedAmountDisplay: string
}

export interface AccountModelUsage {
  model: string
  requestCount: number
  requestCountDisplay: string
  successRate: number | null
  successRateDisplay: string
  inputTokens: number | null
  inputTokensDisplay: string
  outputTokens: number | null
  outputTokensDisplay: string
  cachedTokens: number | null
  cachedTokensDisplay: string
  imageInputTokens: number | null
  imageInputTokensDisplay: string
  imageOutputTokens: number | null
  imageOutputTokensDisplay: string
  imageRequestCount: number
  imageRequestCountDisplay: string
  imageRequestFailedCount: number
  imageRequestFailedCountDisplay: string
  totalTokens: number | null
  totalTokensDisplay: string
  billingAmountUsd: string | null
  billingAmountUsdDisplay: string
  costEstimateStatus: string
  knownCostCount: number
  partialCostCount: number
  unknownCostCount: number
  costs: AccountCurrencyCost[]
  lastUsedAt: string
  lastUsedAtDisplay: string
}

export interface AccountUsage {
  requestCount: number | null
  requestCountDisplay: string
  inputTokens: number | null
  inputTokensDisplay: string
  outputTokens: number | null
  outputTokensDisplay: string
  cachedTokens: number | null
  cachedTokensDisplay: string
  imageInputTokens: number | null
  imageInputTokensDisplay: string
  imageOutputTokens: number | null
  imageOutputTokensDisplay: string
  imageRequestCount: number | null
  imageRequestCountDisplay: string
  imageRequestFailedCount: number | null
  imageRequestFailedCountDisplay: string
  totalTokens: number | null
  totalTokensDisplay: string
  createdTokens: number | null
  createdTokensDisplay: string
  readTokens: number | null
  readTokensDisplay: string
  lastUsedAt: string | null
  lastUsedAtDisplay: string
  costEstimateStatus: string
  knownCostCount: number | null
  partialCostCount: number | null
  unknownCostCount: number | null
  costs: AccountCurrencyCost[]
  models: AccountModelUsage[]
}

export interface Account {
  id: string
  name: string
  provider: string
  resourceRef: string
  email: string | null
  accountId: string | null
  userId: string | null
  label: string | null
  planType: string | null
  authenticationKind: string
  hasRefreshToken: boolean
  status: string
  displayStatus: string
  tokenRefreshing: boolean
  availability: string
  enabled: boolean
  credentialRevision: number
  stateRevision: number | null
  accessTokenExpiresAt: string | null
  accessTokenExpiresAtDisplay: string | null
  refreshTokenExpiresAt: string | null
  nextRefreshAt: string | null
  addedAt: string
  addedAtDisplay: string
  updatedAt: string
  updatedAtDisplay: string
  quota: AccountQuota
  usage: AccountUsage
}

export interface AccountPageMeta {
  page: number
  pageSize: number
  total: number
  totalPages: number
}

export interface AccountSummary {
  total: number
  active: number
  quotaExhausted: number
  unavailable: number
}

export interface AccountListResponse {
  items: Account[]
  page: AccountPageMeta
  summary: AccountSummary
}

export interface AccountRefreshResponse {
  account: Account
  result?: string
  error?: string
}

export interface AccountQuotaResponse {
  account: Account
}

export interface AccountModelsResponse {
  models: Array<{ id: string, label: string }>
}

export interface AccountImportResponse {
  importedCount: number
  accountIds: string[]
}

export interface AccountMutationResponse {
  accountId: string
  credentialRevision?: number
}

export interface AccountDeletionResponse {
  deletedCount: number
  accountIds: string[]
}

export interface AccountOAuthStartResponse {
  flowId: string
  authorizationUrl: string
  expiresAt: string
}

export function getAccounts(data: object) {
  return request<AccountListResponse>({
    url: '/api/admin/accounts',
    method: 'GET',
    params: data,
  })
}

export function exportAccounts(data: object) {
  return request<unknown>({
    url: '/api/admin/accounts/export',
    method: 'GET',
    params: data,
  })
}

export function refreshAccount(data: object) {
  return request<AccountRefreshResponse>({
    url: '/api/admin/accounts/refresh',
    method: 'POST',
    data,
  })
}

export function getAccountQuota(data: object) {
  return request<AccountQuotaResponse>({
    url: '/api/admin/accounts/quota',
    method: 'GET',
    params: data,
  })
}

export function refreshAccountQuota(data: object) {
  return request<AccountQuotaResponse>({
    url: '/api/admin/accounts/quota/refresh',
    method: 'POST',
    data,
  })
}

export function getAccountModels(data: object) {
  return request<AccountModelsResponse>({
    url: '/api/admin/accounts/models',
    method: 'GET',
    params: data,
  })
}

export function refreshAccountModels(data: object) {
  return request<AccountModelsResponse>({
    url: '/api/admin/accounts/models/refresh',
    method: 'POST',
    data,
  })
}

export function importAccounts(data: object) {
  return request<AccountImportResponse>({
    url: '/api/admin/accounts/import',
    method: 'POST',
    data,
  })
}

export function enableAccount(data: object) {
  return request<AccountMutationResponse>({
    url: '/api/admin/accounts/enable',
    method: 'POST',
    data,
  })
}

export function disableAccount(data: object) {
  return request<AccountMutationResponse>({
    url: '/api/admin/accounts/disable',
    method: 'POST',
    data,
  })
}

export function deleteAccounts(data: object) {
  return request<AccountDeletionResponse>({
    url: '/api/admin/accounts/delete',
    method: 'POST',
    data,
  })
}

export function startAccountOAuth(data: object) {
  return request<AccountOAuthStartResponse>({
    url: '/api/admin/accounts/oauth/start',
    method: 'POST',
    data,
  })
}

export function completeAccountOAuth(data: object) {
  return request<AccountMutationResponse>({
    url: '/api/admin/accounts/oauth/complete',
    method: 'POST',
    data,
  })
}
