import { requestJson } from '../request'

// ==================== API Keys 管理 ====================

export interface ClientApiKey {
  id: string
  name: string
  label?: string
  prefix: string
  enabled: boolean
  createdAt: string
  lastUsedAt?: string
}

export interface CreateApiKeyPayload {
  name: string
  label?: string
}

export interface CreateApiKeyResponse {
  key: string
  id: string
  prefix: string
}

export function getApiKeys() {
  return requestJson<ClientApiKey[]>('/api/admin/api-keys')
}

export function createApiKey(payload: CreateApiKeyPayload) {
  return requestJson<CreateApiKeyResponse>('/api/admin/api-keys', {
    method: 'POST',
    data: payload,
  })
}

export function deleteApiKey(keyId: string) {
  return requestJson<void>(`/api/admin/api-keys/${keyId}`, {
    method: 'DELETE',
  })
}

export function updateApiKeyLabel(keyId: string, label: string | null) {
  return requestJson<ClientApiKey>(`/api/admin/api-keys/${keyId}/label`, {
    method: 'PATCH',
    data: { label },
  })
}

export function updateApiKeyStatus(keyId: string, enabled: boolean) {
  return requestJson<ClientApiKey>(`/api/admin/api-keys/${keyId}/status`, {
    method: 'PATCH',
    data: { enabled },
  })
}

export function batchDeleteApiKeys(keyIds: string[]) {
  return requestJson<{ deleted: number }>('/api/admin/api-keys/batch-delete', {
    method: 'POST',
    data: { keyIds },
  })
}
