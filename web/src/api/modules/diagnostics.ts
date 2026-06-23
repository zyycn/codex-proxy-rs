import { requestJson } from '../request'

// ==================== 系统诊断 ====================

export interface DiagnosticsInfo {
  version?: string
  uptime?: number
  environment?: string
  database?: {
    connected: boolean
    path?: string
  }
  accounts?: {
    total?: number
    active?: number
    disabled?: number
    banned?: number
    pool?: {
      total: number
      active: number
      expired: number
      quotaExhausted: number
      refreshing: number
      disabled: number
      banned: number
    }
    capacity?: {
      maxConcurrentPerAccount: number
      totalSlots: number
      usedSlots: number
      availableSlots: number
    }
  }
  settings?: {
    rotationStrategy?: string | null
  }
  transport?: {
    backendBaseUrl?: string
    fingerprint?: Record<string, unknown>
  }
  requests?: {
    total: number
    today: number
  }
}

export function getDiagnostics() {
  return requestJson<DiagnosticsInfo>('/api/admin/diagnostics')
}
