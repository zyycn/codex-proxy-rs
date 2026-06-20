import { requestJson } from '../request'

// ==================== 使用统计 ====================

export interface UsageSummary {
  totalRequests: number
  totalInputTokens: number
  totalOutputTokens: number
  todayRequests: number
  todayInputTokens: number
  todayOutputTokens: number
  activeAccounts: number
  errorRate: number
  avgLatencyMs: number
}

export function getUsageSummary() {
  return requestJson<UsageSummary>('/api/admin/usage-stats/summary')
}

export interface UsageStats {
  hourly: Array<{
    hour: string
    requests: number
    inputTokens: number
    outputTokens: number
    errors: number
  }>
  daily: Array<{
    date: string
    requests: number
    inputTokens: number
    outputTokens: number
    errors: number
  }>
}

export function getUsageStats(period: 'today' | 'week' | 'month' = 'today') {
  return requestJson<UsageStats>(`/api/admin/usage-stats?period=${period}`)
}
