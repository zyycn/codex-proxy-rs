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

export function getApiKeys(data: object) {
  return request<ApiKeyListResponse>({
    url: '/api/admin/client-keys',
    method: 'GET',
    params: data,
  })
}

export function createApiKey(data: object) {
  return request<ApiKeyCreateResponse>({
    url: '/api/admin/client-keys/create',
    method: 'POST',
    data,
  })
}

export function revealApiKey(data: object) {
  return request<ApiKeyRevealResponse>({
    url: '/api/admin/client-keys/reveal',
    method: 'GET',
    params: data,
  })
}

export function deleteApiKey(data: object) {
  return request<ApiKeyMutationResponse>({
    url: '/api/admin/client-keys/delete',
    method: 'POST',
    data,
  })
}

export function disableApiKey(data: object) {
  return request<ApiKeyMutationResponse>({
    url: '/api/admin/client-keys/disable',
    method: 'POST',
    data,
  })
}

export function enableApiKey(data: object) {
  return request<ApiKeyMutationResponse>({
    url: '/api/admin/client-keys/enable',
    method: 'POST',
    data,
  })
}
