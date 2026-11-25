import type { RequestOptions } from '../request'
import type { ProviderRequestProfile, ProviderRequestProfiles, ProviderRequestProfileUpdates } from '../types/request-profile'
import type { AccountGroupRef } from './groups'
import request from '../request'

export type ApiKeyRoutingScope = 'all' | 'groups'
export type ApiKeyBudgetPeriod = 'daily' | 'weekly' | 'all'

export interface ApiKeyPeerSharing {
  enabled: boolean
  sharePool: boolean
  shareQuota: boolean
  sharePlans: boolean
}

export interface ApiKey {
  peerSharing: ApiKeyPeerSharing
  providerRequestProfileOverrides: ProviderRequestProfiles

  id: string
  name: string
  label: string | null
  prefix: string
  enabled: boolean
  maxConcurrency: number
  requestsPerMinute: number
  dailyLimitUsd: string
  weeklyLimitUsd: string
  dailyUsedUsd: string
  weeklyUsedUsd: string
  dailyResetsAt: string | null
  dailyResetsAtDisplay: string | null
  weeklyResetsAt: string | null
  weeklyResetsAtDisplay: string | null
  createdAt: string
  createdAtDisplay: string
  updatedAt: string
  updatedAtDisplay: string
  lastUsedAt: string | null
  lastUsedAtDisplay: string
  lastUsedAtFullDisplay: string | null
  routingScope: ApiKeyRoutingScope
  groups: AccountGroupRef[]
  providerKinds: string[]
}

export interface ApiKeyListResponse {
  items: ApiKey[]
  nextCursor: string | null
  total: number
}

export interface ApiKeyCreateResponse {
  id: string
  prefix: string
  plaintextKey: string
}

export interface ApiKeyRevealResponse {
  id: string
  plaintextKey: string
}

export interface ApiKeyMutationResponse {
  id: string
}

// 请求参数类型：仅定义 API 边界的形状，调用方不依赖显式声明。
interface ApiKeyListParams {
  cursor?: string
  limit: number
  search?: string
  sortBy?: string
  sortDirection?: string
}

export interface ApiKeyWriteParam {
  peerSharing: ApiKeyPeerSharing
  name: string
  label: string | null
  groupIds: string[]
  maxConcurrency: number
  requestsPerMinute: number
  dailyLimitUsd: string
  weeklyLimitUsd: string
}

interface ApiKeyUpdateParam extends ApiKeyWriteParam {
  id: string
  providerRequestProfileOverrides: Record<string, ProviderRequestProfile | null>
}

interface ApiKeyCreateParam extends ApiKeyWriteParam {
  providerRequestProfileOverrides: ProviderRequestProfiles
  customKey?: string
}

interface ApiKeyIdParam {
  id: string
}

interface ApiKeyResetBudgetParam extends ApiKeyIdParam {
  period: ApiKeyBudgetPeriod
}

export function getApiKeys(data: ApiKeyListParams, options: RequestOptions = {}) {
  return request<ApiKeyListResponse>({
    url: '/api/admin/client-keys',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function createApiKey(data: ApiKeyCreateParam) {
  return request<ApiKeyCreateResponse>({
    url: '/api/admin/client-keys/create',
    method: 'POST',
    data,
  })
}

export interface ApiKeyBatchChanges extends Partial<Omit<ApiKeyWriteParam, 'name' | 'label' | 'peerSharing'>> {
  enabled?: boolean
  peerSharing?: Partial<ApiKeyPeerSharing>
  providerRequestProfileOverrides?: ProviderRequestProfileUpdates
  installationSeed?: string
}

export function batchUpdateApiKeys(data: ApiKeyBatchChanges & { ids: string[] }) {
  return request<{ updated: number, configRevision: number }>({
    url: '/api/admin/client-keys/batch-update',
    method: 'POST',
    data,
  })
}

export function updateApiKey(data: ApiKeyUpdateParam) {
  return request<ApiKeyMutationResponse>({
    url: '/api/admin/client-keys/update',
    method: 'POST',
    data,
  })
}

export function revealApiKey(data: ApiKeyIdParam, options: RequestOptions = {}) {
  return request<ApiKeyRevealResponse>({
    url: '/api/admin/client-keys/reveal',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function deleteApiKey(data: ApiKeyIdParam) {
  return request<ApiKeyMutationResponse>({
    url: '/api/admin/client-keys/delete',
    method: 'POST',
    data,
  })
}

export function resetApiKeyBudget(data: ApiKeyResetBudgetParam) {
  return request<ApiKeyMutationResponse>({
    url: '/api/admin/client-keys/reset-budget',
    method: 'POST',
    data,
  })
}

export function disableApiKey(data: ApiKeyIdParam) {
  return request<ApiKeyMutationResponse>({
    url: '/api/admin/client-keys/disable',
    method: 'POST',
    data,
  })
}

export function enableApiKey(data: ApiKeyIdParam) {
  return request<ApiKeyMutationResponse>({
    url: '/api/admin/client-keys/enable',
    method: 'POST',
    data,
  })
}

export type KeyUsageMetric = 'costUsd' | 'tokens' | 'requests'
export type KeyStatisticsPeriod = '7d' | '30d'
export interface KeyUsageTotals {
  requests: number
  tokens: number | null
  costUsd: string | null
  hasMissingCost: boolean
  hasEstimatedCost: boolean
}
export interface KeyModelRow {
  model: string
  totals: KeyUsageTotals
  activeDays: number
  peakRequests: number
}
export interface KeyStatistics {
  budget: Pick<ApiKey, 'dailyLimitUsd' | 'weeklyLimitUsd' | 'dailyUsedUsd' | 'weeklyUsedUsd' | 'dailyResetsAt' | 'weeklyResetsAt' | 'dailyResetsAtDisplay' | 'weeklyResetsAtDisplay'>
  dates: string[]
  asOf: number
  totals: KeyUsageTotals
  modelCount: number
  series: { model: string | null, points: { date: string, value: string | null }[] }[]
}
export function getKeyStatistics(id: string, period: KeyStatisticsPeriod, metric: KeyUsageMetric, asOf: number | undefined, options: RequestOptions) {
  return request<KeyStatistics>({
    url: '/api/admin/client-keys/statistics',
    method: 'GET',
    params: { id, period, metric, asOf },
    ...options,
  })
}
export function getKeyModelTotals(id: string, period: KeyStatisticsPeriod, asOf: number, options: RequestOptions) {
  return request<KeyModelRow[]>({
    url: '/api/admin/client-keys/statistics/models',
    method: 'GET',
    params: { id, period, asOf },
    ...options,
  })
}

export interface BudgetReferenceAccount {
  id: string
  name: string
  email: string | null
  provider: string
}

export function getBudgetReferenceAccounts(id: string, search: string, cursor: string | undefined, options: RequestOptions) {
  return request<{ items: BudgetReferenceAccount[], nextCursor: string | null }>({
    url: '/api/admin/client-keys/budget-reference-accounts',
    method: 'GET',
    params: { id, search: search || undefined, cursor },
    ...options,
  })
}

export interface KeyReferenceWindow {
  key: string
  label: string
  period: 'daily' | 'weekly'
  startsAt: string
  startsAtDisplay: string
  endsAt: string
  endsAtDisplay: string
}

export interface KeyAlignmentPreview {
  revision: number
  windows: KeyReferenceWindow[]
}

export function previewKeyAlignment(id: string, accountId: string, options: RequestOptions) {
  return request<KeyAlignmentPreview>({
    url: '/api/admin/client-keys/budget-alignment',
    method: 'GET',
    params: { id, accountId },
    ...options,
  })
}

export function alignKeyBudget(data: { id: string, accountId: string, revision: number, windowKey: string, startsAt: string, endsAt: string }) {
  return request<number>({ url: '/api/admin/client-keys/budget-alignment', method: 'POST', data })
}
