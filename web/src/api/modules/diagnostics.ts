import { requestJson } from '../request'

// ==================== 系统诊断 ====================

export interface DiagnosticsInfo {
  version: string
  uptime: number
  environment: string
  database: {
    connected: boolean
    path?: string
  }
  accounts: {
    total: number
    active: number
    disabled: number
    banned: number
  }
  requests: {
    total: number
    today: number
  }
}

export function getDiagnostics() {
  return requestJson<DiagnosticsInfo>('/api/admin/diagnostics')
}
