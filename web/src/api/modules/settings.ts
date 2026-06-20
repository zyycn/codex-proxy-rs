import { requestJson } from '../request'

// ==================== 设置管理 ====================

export interface Settings {
  logging: {
    enabled: boolean
    level: string
  }
  quota: {
    warningThresholds: {
      requestsRemaining: number
      tokensRemaining: number
    }
  }
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
