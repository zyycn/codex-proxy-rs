import { requestJson, requestPageJson } from '../request'

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

export async function getApiKeys() {
  const result = await requestPageJson<ClientApiKey>('/api/admin/keys')
  return result.items
}

export function createApiKey(payload: CreateApiKeyPayload) {
  return requestJson<CreateApiKeyResponse>('/api/admin/keys', {
    method: 'POST',
    data: payload,
  })
}

export function deleteApiKey(keyId: string) {
  return requestJson<{ deleted: number }>('/api/admin/keys/delete', {
    method: 'POST',
    data: { ids: [keyId] },
  })
}

export function updateApiKeyLabel(keyId: string, label: string | null) {
  return requestJson<ClientApiKey>('/api/admin/keys/update', {
    method: 'POST',
    data: { id: keyId, label },
  })
}

export function updateApiKeyStatus(keyId: string, enabled: boolean) {
  return requestJson<ClientApiKey>('/api/admin/keys/update', {
    method: 'POST',
    data: { id: keyId, status: enabled ? 'active' : 'disabled' },
  })
}

export function batchDeleteApiKeys(keyIds: string[]) {
  return requestJson<{ deleted: number }>('/api/admin/keys/delete', {
    method: 'POST',
    data: { ids: keyIds },
  })
}
