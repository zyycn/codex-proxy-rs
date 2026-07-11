import request, { type ApiPayload } from '../request'

// ==================== API Keys 管理 ====================

export interface ApiKeyListParams {
  page: number
  pageSize: number
  search?: string
}

export interface ApiKeyPageMeta {
  page: number
  pageSize: number
  total: number
  totalPages: number
}

export interface ClientApiKey {
  id: string
  name: string
  label: string | null
  prefix: string
  key: string
  enabled: boolean
  createdAt: string
  createdAtDisplay: string
  lastUsedAt: string | null
  lastUsedAtDisplay: string
}

export interface ApiKeyListResult {
  items: ClientApiKey[]
  page: ApiKeyPageMeta
}

export function getApiKeys(params: ApiKeyListParams) {
  return request<ApiKeyListResult>({
    url: '/api/admin/keys',
    method: 'GET',
    params,
  })
}

export function createApiKey(data: ApiPayload) {
  return request<ClientApiKey>({
    url: '/api/admin/keys',
    method: 'POST',
    data,
  })
}

export function deleteApiKeys(data: ApiPayload) {
  return request({
    url: '/api/admin/keys/delete',
    method: 'POST',
    data,
  })
}

export function updateApiKey(data: ApiPayload) {
  return request<ClientApiKey>({
    url: '/api/admin/keys/update',
    method: 'POST',
    data,
  })
}
