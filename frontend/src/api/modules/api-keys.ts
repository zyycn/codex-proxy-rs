import type { RequestOptions } from '../request'
import type { AccountGroupRef } from './account-groups'
import type { ClientProfileSelection, XaiClientProfileSelection } from './client-profiles'
import request from '../request'

export type ApiKeyRoutingScope = 'all' | 'groups'
export type ApiKeyBudgetPeriod = 'daily' | 'weekly' | 'all'

export interface ApiKey {
  openaiClientProfileOverride: ClientProfileSelection | null
  xaiClientProfileOverride: XaiClientProfileSelection | null

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
  weeklyResetsAt: string | null
  createdAt: string
  updatedAt: string
  lastUsedAt: string | null
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
  openaiClientProfileOverride: ClientProfileSelection | null
  xaiClientProfileOverride: XaiClientProfileSelection | null

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
}

interface ApiKeyCreateParam extends ApiKeyWriteParam {
  customKey?: string
}

interface ApiKeyIdParam {
  id: string
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

export function updateApiKey(data: ApiKeyUpdateParam) {
  return request<ApiKeyMutationResponse>({
    url: '/api/admin/client-keys/update',
    method: 'POST',
    data,
  })
}

export function revealApiKey(data: ApiKeyIdParam) {
  return request<ApiKeyRevealResponse>({
    url: '/api/admin/client-keys/reveal',
    method: 'GET',
    params: data,
  })
}

export function deleteApiKey(data: ApiKeyIdParam) {
  return request<ApiKeyMutationResponse>({
    url: '/api/admin/client-keys/delete',
    method: 'POST',
    data,
  })
}

export function resetApiKeyBudget(data: ApiKeyIdParam & { period: ApiKeyBudgetPeriod }) {
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
