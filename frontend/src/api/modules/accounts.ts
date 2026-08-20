import type { AccountGroupRef } from './account-groups'
import request from '../request'

export type AccountStatus
  = 'normal' | 'quota_exhausted' | 'rate_limited' | 'disabled' | 'error'

export type AccountErrorReason
  = 'account_unverified'
    | 'access_token_expired'
    | 'credential_expired'
    | 'credential_invalid'
    | 'account_banned'

export interface AccountQuotaWindow {
  key: string
  group: string
  limitId: string | null
  limitName: string | null
  role: 'primary' | 'secondary' | 'monthly' | null
  windowSeconds: number | null
  labelDisplay: string
  windowLabelDisplay: string
  usedPercent: number | null
  usedPercentDisplay: string
  limitReached: boolean
  localUsage?: unknown
  resetAtDisplay: string
}

export interface AccountQuota {
  refreshedAtDisplay: string
  limitReached: boolean
  // 429 临时限流（Redis 冷却）到期时间；非限流中为 null。
  rateLimitedUntil: string | null
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
  status: AccountStatus
  errorReason: AccountErrorReason | null
  errorMessage: string | null
  enabled: boolean
  concurrencyLimit: number | null
  weight: number
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
  groups: AccountGroupRef[]
}

export interface AccountPageMeta {
  page: number
  pageSize: number
  total: number
  totalPages: number
}

export interface AccountSummary {
  total: number
  normal: number
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

export interface AccountRefreshResponse {
  account: Account
  result?: string
  error?: string
}

export interface AccountQuotaResponse {
  account: Account
}

export interface AccountResetCredit {
  id: string
  status: string | null
  title: string | null
  expiresAt: string | null
  resetType: string | null
}

export interface AccountResetCreditsResponse {
  availableCount: number
  credits: AccountResetCredit[]
}

export interface AccountResetCreditResultResponse {
  code: string
  credit: AccountResetCredit | null
}

export interface AccountModelsResponse {
  models: Array<{ id: string, label: string }>
}

export interface AccountImportResponse {
  importedCount: number
  accountIds: string[]
}

export interface AccountOAuthCompleteResponse {
  accountId: string
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

export interface AccountOAuthStartResponse {
  flowId: string
  authorizationUrl: string
  expiresAt: string
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

interface AccountIdParam {
  accountId: string
}

interface AccountResetCreditConsumeParam extends AccountIdParam {
  creditId?: string
  redeemRequestId: string
}

interface AccountUpdateParam {
  accountId: string
  enabled: boolean
  concurrencyLimit: number | null
  weight: number
  groupIds: string[]
}

interface AccountBatchUpdateParam {
  accountIds: string[]
  enabled: boolean
  concurrencyLimit: number | null
  weight: number
  groupIds: string[]
}

interface AccountDeleteParams {
  provider: string
  accountIds: string[]
}

interface AccountImportParam {
  provider: string
  data: unknown
}

interface AccountOAuthStartParam {
  provider: string
  name: string
  accountId?: string
}

interface AccountOAuthCompleteParam {
  provider: string
  flowId: string
  callbackUrl: string
}

interface AccountExportParam {
  accountIds: string
  confirm: string
}

export function getAccounts(data: AccountListParams) {
  return request<AccountListResponse>({
    url: '/api/admin/accounts',
    method: 'GET',
    params: data,
  })
}

export function exportAccounts(data: AccountExportParam) {
  return request<unknown>({
    url: '/api/admin/accounts/export',
    method: 'GET',
    params: data,
  })
}

export function refreshAccount(data: AccountIdParam) {
  return request<AccountRefreshResponse>({
    url: '/api/admin/accounts/refresh',
    method: 'POST',
    data,
  })
}

export function recoverAccount(data: AccountIdParam) {
  return request<AccountRefreshResponse>({
    url: '/api/admin/accounts/recover',
    method: 'POST',
    data,
  })
}

export function getAccountQuota(data: AccountIdParam) {
  return request<AccountQuotaResponse>({
    url: '/api/admin/accounts/quota',
    method: 'GET',
    params: data,
  })
}

export function refreshAccountQuota(data: AccountIdParam) {
  return request<AccountQuotaResponse>({
    url: '/api/admin/accounts/quota/refresh',
    method: 'POST',
    data,
  })
}

export function getAccountResetCredits(data: AccountIdParam) {
  return request<AccountResetCreditsResponse>({
    url: '/api/admin/accounts/reset-credits',
    method: 'GET',
    params: data,
  })
}

export function consumeAccountResetCredit(data: AccountResetCreditConsumeParam) {
  return request<AccountResetCreditResultResponse>({
    url: '/api/admin/accounts/reset-credits',
    method: 'POST',
    data,
  })
}

export function getAccountModels(data: AccountIdParam) {
  return request<AccountModelsResponse>({
    url: '/api/admin/accounts/models',
    method: 'GET',
    params: data,
  })
}

export function refreshAccountModels(data: AccountIdParam) {
  return request<AccountModelsResponse>({
    url: '/api/admin/accounts/models/refresh',
    method: 'POST',
    data,
  })
}

export function importAccounts(data: AccountImportParam) {
  return request<AccountImportResponse>({
    url: '/api/admin/accounts/import',
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

export function deleteAccounts(data: AccountDeleteParams) {
  return request<AccountDeletionResponse>({
    url: '/api/admin/accounts/delete',
    method: 'POST',
    data,
  })
}

export function startAccountOAuth(data: AccountOAuthStartParam) {
  return request<AccountOAuthStartResponse>({
    url: '/api/admin/accounts/oauth/start',
    method: 'POST',
    data,
  })
}

export function completeAccountOAuth(data: AccountOAuthCompleteParam) {
  return request<AccountOAuthCompleteResponse>({
    url: '/api/admin/accounts/oauth/complete',
    method: 'POST',
    data,
  })
}
