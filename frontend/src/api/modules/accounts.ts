import type { RequestOptions } from '../request'
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
  rateLimitReason: 'upstream_rate_limit' | 'capacity_freeze' | null
  recoveryProbeRequired: boolean
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
  windowLabelDisplay: string
  requestCount: number | null
  requestCountDisplay: string
  inputTokens: number | null
  inputTokensDisplay: string
  outputTokens: number | null
  outputTokensDisplay: string
  cachedTokens: number | null
  cachedTokensDisplay: string
  reasoningTokens: number | null
  reasoningTokensDisplay: string
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

export interface AccountModelAccess {
  mode: 'all' | 'allowlist' | 'denylist'
  models: string[]
}

export interface Account {
  outboundProxyEndpoint: string | null
  id: string
  name: string
  notes: string | null
  provider: string
  resourceRef: string
  email: string | null
  accountId: string | null
  userId: string | null
  label: string | null
  planType: string | null
  planTypeDisplay: string
  authenticationKind: string
  hasRefreshToken: boolean
  status: AccountStatus
  errorReason: AccountErrorReason | null
  errorMessage: string | null
  enabled: boolean
  concurrencyLimit: number | null
  weight: number
  modelAccess: AccountModelAccess
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

export interface AccountQuotaForecast {
  period: 'weekly' | 'monthly'
  targetDays: number
  extrapolated: boolean
  source: {
    label: string
    usedPercent: number | null
    usedPercentDisplay: string
    observedAt: string | null
    observedAtDisplay: string
    resetAt: string
    tokensDisplay: string
    usdDisplay: string
  } | null
  unavailableReason: string | null
  lowSample: boolean
  incompleteCost: boolean
  incompleteTokens: boolean
  estimatedTokens: number | null
  estimatedTokensDisplay: string
  estimatedUsd: number | null
  estimatedUsdDisplay: string
  remainingTokens: number | null
  remainingTokensDisplay: string
  remainingUsd: number | null
  remainingUsdDisplay: string
}

export interface AccountQuotaForecastResponse {
  accountId: string
  generatedAt: string
  forecasts: AccountQuotaForecast[]
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

export interface AccountProfileStatisticsSummary {
  totalTextTokens: number | null
  peakTokens: number | null
  longestTaskDurationMs: number | null
  currentStreakDays: number | null
  longestStreakDays: number | null
}

export interface AccountProfileDailyUsage {
  date: string
  tokens: number
}

export interface AccountProfileInvocation {
  type: string
  pluginId: string | null
  pluginName: string | null
  skillId: string | null
  skillName: string | null
  usageCount: number | null
}

export interface AccountProfileActivityInsights {
  fastModePercent: number | null
  reasoningEffort: string | null
  reasoningEffortPercent: number | null
  skillsExplored: number | null
  totalSkillsUsed: number | null
  totalThreads: number | null
  invocations: AccountProfileInvocation[] | null
}

export interface AccountSubscription {
  startsAt: string | null
  expiresAt: string
  willRenew: boolean | null
  billingPeriod: string | null
  billingCurrency: string | null
  observedAt: string
}

export interface AccountProfileStatisticsResponse {
  displayName: string | null
  username: string | null
  imageUrl: string | null
  hasStatsError: boolean
  summary: AccountProfileStatisticsSummary
  dailyUsage: AccountProfileDailyUsage[] | null
  activityInsights: AccountProfileActivityInsights
}

export interface AccountPersonalInfoResponse {
  profile: AccountProfileStatisticsResponse | null
  profileError: string | null
  subscription: AccountSubscription | null
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

export type ImportItemStatus = 'pending' | 'running' | 'succeeded' | 'failed' | 'unknown' | 'skipped'

export interface AccountImportTask {
  taskId: string
  createdAt: string
  finishedAt: string | null
  stopRequested: boolean
  total: number
  counts: Record<ImportItemStatus, number> & { importedAccounts: number }
}

export interface AccountImportTaskItem {
  index: number
  provider: string
  status: ImportItemStatus
  accountIds: string[]
  message: string | null
}

export interface AccountImportTaskDetail extends AccountImportTask {
  items: AccountImportTaskItem[]
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
  outboundProxyUrl?: string
  outboundProxyId?: string
  accountId: string
  notes?: string
  enabled: boolean
  concurrencyLimit: number | null
  weight: number
  modelAccess?: AccountModelAccess
  groupIds: string[]
}

interface AccountBatchUpdateParam {
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

interface AccountImportSettings {
  notes?: string
  enabled: boolean
  concurrencyLimit: number | null
  weight: number
  modelAccess?: AccountModelAccess
  groupIds: string[]
}

interface AccountImportParam {
  outboundProxyId?: string
  settings?: AccountImportSettings
  provider: string
  data: unknown
}

interface AccountImportTaskIdParam {
  taskId: string
}

interface CreateAccountImportTaskParam {
  submissionId: string
  items: AccountImportParam[]
}

interface AccountOAuthStartParam {
  outboundProxyUrl?: string
  outboundProxyId?: string
  provider: string
  name: string
  accountId?: string
}

interface AccountOAuthCompleteParam {
  settings?: AccountImportSettings
  provider: string
  flowId: string
  callbackUrl: string
}

interface AccountExportParam {
  accountIds: string
  confirm: string
}

export function getAccounts(data: AccountListParams, options: RequestOptions = {}) {
  return request<AccountListResponse>({
    url: '/api/admin/accounts',
    method: 'GET',
    params: data,
    ...options,
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

export function getAccountPersonalInfo(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountPersonalInfoResponse>({
    url: '/api/admin/accounts/personal-info',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getAccountQuotaForecast(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountQuotaForecastResponse>({
    url: '/api/admin/accounts/quota-forecast',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function accountProfileAvatarUrl(accountId: string, sourceUrl: string) {
  const params = new URLSearchParams({
    accountId,
    version: stableAvatarVersion(sourceUrl),
  })
  return `/api/admin/accounts/profile-avatar?${params.toString()}`
}

function stableAvatarVersion(value: string) {
  let hash = 0
  for (const character of value)
    hash = (hash * 33 + (character.codePointAt(0) ?? 0)) % 2_147_483_647
  return hash.toString(36)
}

export function refreshAccountQuota(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountQuotaResponse>({
    url: '/api/admin/accounts/quota/refresh',
    method: 'POST',
    data,
    ...options,
  })
}

export function getAccountResetCredits(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountResetCreditsResponse>({
    url: '/api/admin/accounts/reset-credits',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function consumeAccountResetCredit(data: AccountResetCreditConsumeParam, options: RequestOptions = {}) {
  return request<AccountResetCreditResultResponse>({
    url: '/api/admin/accounts/reset-credits',
    method: 'POST',
    data,
    ...options,
  })
}

export interface UpdateAccountWebTokenParam {
  accountId: string
  webAccessToken: string | null
}

export function updateAccountWebToken(data: UpdateAccountWebTokenParam, options: RequestOptions = {}) {
  return request<AccountRefreshResponse>({
    url: '/api/admin/accounts/web-token',
    method: 'POST',
    data,
    ...options,
  })
}

export function getAccountModels(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountModelsResponse>({
    url: '/api/admin/accounts/models',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function refreshAccountModels(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountModelsResponse>({
    url: '/api/admin/accounts/models/refresh',
    method: 'POST',
    data,
    ...options,
  })
}

export function importAccounts(data: AccountImportParam, options: RequestOptions = {}) {
  return request<AccountImportResponse>({
    url: '/api/admin/accounts/import',
    method: 'POST',
    data,
    ...options,
  })
}

export function createAccountImportTask(data: CreateAccountImportTaskParam) {
  return request<AccountImportTask>({
    url: '/api/admin/accounts/import-tasks',
    method: 'POST',
    data,
  })
}

export function getAccountImportTasks(options: RequestOptions = {}) {
  return request<{ items: AccountImportTask[] }>({
    url: '/api/admin/accounts/import-tasks',
    method: 'GET',
    ...options,
  })
}

export function getAccountImportTask(data: AccountImportTaskIdParam, options: RequestOptions = {}) {
  return request<AccountImportTaskDetail>({
    url: '/api/admin/accounts/import-tasks/detail',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function stopAccountImportTask(data: AccountImportTaskIdParam) {
  return request<AccountImportTaskDetail>({
    url: '/api/admin/accounts/import-tasks/stop',
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

export interface ApiKeyConfiguration {
  base_url: string
  transport: 'http' | 'prefer_websocket'
}

export function getAccountDetail(data: AccountIdParam, options: RequestOptions = {}) {
  return request<{ account: Account, credentialConfiguration?: ApiKeyConfiguration }>({
    url: '/api/admin/accounts/detail',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function updateAccountApiKey(data: { accountId: string, baseUrl: string, transport: ApiKeyConfiguration['transport'], apiKey?: string, settings?: AccountUpdateParam }) {
  return request<{ accountId: string }>({
    url: '/api/admin/accounts/rotate',
    method: 'POST',
    data: { provider: 'openai', ...data },
  })
}
