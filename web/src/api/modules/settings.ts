import { requestJson } from '../request'

// ==================== 设置管理 ====================

export interface QuotaWarningThresholds {
  primary: number[]
  secondary: number[]
}

export interface Settings {
  defaultModel: string
  defaultReasoningEffort?: string | null
  serviceTier?: string | null
  modelAliases?: Record<string, string>
  refreshEnabled: boolean
  refreshMarginSeconds?: number
  refreshConcurrency?: number
  maxConcurrentPerAccount: number
  requestIntervalMs: number
  rotationStrategy: string
  tierPriority?: string[]
  quotaRefreshIntervalMinutes?: number
  quotaWarningThresholds: QuotaWarningThresholds
  quotaSkipExhausted: boolean
  logsEnabled: boolean
  logsCapacity: number
  logsCaptureBody?: boolean
  usageHistoryRetentionDays?: number | null
  [key: string]: unknown
}

export function getSettings() {
  return requestJson<Settings>('/api/admin/settings')
}

export function updateSettings(settings: Partial<Settings>) {
  return requestJson<Settings>('/api/admin/settings', {
    method: 'PATCH',
    data: settings,
  })
}
