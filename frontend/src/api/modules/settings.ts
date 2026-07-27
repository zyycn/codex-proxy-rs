import request from '../request'

export type RotationStrategy = 'smart' | 'quota_reset_priority' | 'round_robin' | 'sticky'

export interface RuntimeSettings {
  modelMappings: Record<string, string>
  refreshMarginSeconds: number
  refreshConcurrency: number
  maxConcurrentPerAccount: number
  requestIntervalMs: number
  rotationStrategy: RotationStrategy
  usageRetentionDays: number
  opsEventRetentionDays: number
  auditRetentionDays: number
  updatedAt: string
}

export interface AdminApiKeyStatus {
  exists: boolean
}

export interface RegeneratedAdminApiKey {
  key: string
}

export interface DeletedAdminApiKey {
  message: string
}

export function getSettings() {
  return request<RuntimeSettings>({
    url: '/api/admin/settings',
    method: 'GET',
  })
}

export function updateSettings(data: object) {
  return request<RuntimeSettings>({
    url: '/api/admin/settings/update',
    method: 'POST',
    data,
  })
}

export function getAdminApiKeyStatus() {
  return request<AdminApiKeyStatus>({
    url: '/api/admin/settings/admin-api-key',
    method: 'GET',
  })
}

export function regenerateAdminApiKey() {
  return request<RegeneratedAdminApiKey>({
    url: '/api/admin/settings/admin-api-key/regenerate',
    method: 'POST',
  })
}

export function deleteAdminApiKey() {
  return request<DeletedAdminApiKey>({
    url: '/api/admin/settings/admin-api-key/delete',
    method: 'POST',
  })
}
