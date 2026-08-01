import request from '../request'

export interface ApiKey {
  id: string
  name: string
  label: string | null
  providerKind: string
  prefix: string
  enabled: boolean
  maxConcurrency: number
  requestsPerMinute: number
  createdAt: string
  updatedAt: string
  lastUsedAt: string | null
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

interface ApiKeyCreateParam {
  name: string
  label?: string
  providerKind: string
  maxConcurrency: number
  requestsPerMinute: number
}

interface ApiKeyIdParam {
  id: string
}

export function getApiKeys(data: ApiKeyListParams) {
  return request<ApiKeyListResponse>({
    url: '/api/admin/client-keys',
    method: 'GET',
    params: data,
  })
}

export function createApiKey(data: ApiKeyCreateParam) {
  return request<ApiKeyCreateResponse>({
    url: '/api/admin/client-keys/create',
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
