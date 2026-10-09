import type { RequestOptions } from '../../request'
import type { CodexPrivacyPolicy } from './privacy'
import type { ClientProfileSelection, ProviderRequestProfiles, ProviderRequestProfileUpdates, XaiClientProfileSelection } from './profiles'
import request from '../../request'

export * from './backups'
export * from './pricing'
export * from './privacy'
export * from './system'

export interface RequestLocation {
  country: string
  region: string
  city: string
  timezone: string
}

export type AccountAffinity = 'relaxed' | 'preferred' | 'strict'

export type RotationStrategy = 'smart' | 'quota_reset_priority' | 'round_robin' | 'sticky'

export interface SmartSchedulingConfig {
  loadWeight: number
  quotaWeight: number
  healthWeight: number
  latencyWeight: number
  resetWeight: number
  queueWeight: number
  preferHigherWeight: boolean
}

export interface RuntimeSettings {
  configRevision: number
  smartScheduling: SmartSchedulingConfig
  smartSchedulingDefaults: SmartSchedulingConfig
  providerRequestProfiles: ProviderRequestProfiles
  openaiClientProfile: ClientProfileSelection | null
  xaiClientProfile: XaiClientProfileSelection | null

  codexPrivacyPolicy: CodexPrivacyPolicy
  requestLocationEnabled: boolean
  requestLocation: RequestLocation
  modelMappings: Record<string, string>
  refreshMarginSeconds: number
  refreshConcurrency: number
  maxConcurrentPerAccount: number
  openaiGuardianReservedConcurrency: number
  openaiAccountAffinity: AccountAffinity
  maxAccountRotations: number
  openaiSessionAffinityTtlHours: number
  requestIntervalMs: number
  maxWaitingPerKey: number
  maxWaitingPerAccount: number
  concurrencyWaitTimeoutSeconds: number
  responsesMaxDecompressedBodyBytes: number
  rotationStrategy: RotationStrategy
  minCodexDesktopVersion: string | null
  minCodexCliVersion: string | null
  usageRetentionDays: number
  opsEventRetentionDays: number
  auditRetentionDays: number
  accountAutoFreezeEnabled: boolean
  accountAutoFreezeThreshold: number
  accountAutoFreezeWindowSeconds: number
  accountAutoFreezeDurationSeconds: number
  accountAutoFreezeProbeEnabled: boolean
  accountAutoFreezeProbeModel: string | null
  accountAutoFreezeAdaptiveConcurrency: boolean
  accountWarmupEnabled: boolean
  accountWarmupScheduleTime: string
  accountWarmupModel: string | null
  updatedAt: string
  updatedAtDisplay: string
}

export type ClientArchitecture = 'x64' | 'arm64'
export type ClientDownloadSource = 'microsoft_store' | 'official_openai'

export interface ClientDownloadPackage {
  architecture: ClientArchitecture
  source: ClientDownloadSource
  version: string | null
  fileName: string
  sizeBytes: number | null
  downloadUrl: string
  expiresAt: string | null
  expiresAtDisplay: string | null
}

export interface CodexDesktopWindowsDownloads {
  resolvedAt: string
  resolvedAtDisplay: string
  cached: boolean
  warning: string | null
  packages: ClientDownloadPackage[]
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

export function getSettings(options: RequestOptions = {}) {
  return request<RuntimeSettings>({
    url: '/api/admin/settings',
    method: 'GET',
    ...options,
  })
}

type UpdateSettingsParam = Omit<RuntimeSettings, 'updatedAt' | 'updatedAtDisplay' | 'smartSchedulingDefaults' | 'openaiClientProfile' | 'xaiClientProfile' | 'providerRequestProfiles'> & {
  providerRequestProfiles: ProviderRequestProfileUpdates
}

export function updateSettings(data: UpdateSettingsParam) {
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

export function getCodexDesktopWindowsDownloads(data: { refresh?: boolean } = {}) {
  return request<CodexDesktopWindowsDownloads>({
    url: '/api/admin/settings/client-downloads/codex-desktop/windows',
    method: 'GET',
    params: data,
  })
}
