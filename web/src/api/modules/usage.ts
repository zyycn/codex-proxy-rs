import { requestJson } from '../request'

// ==================== 使用统计 ====================

export interface UsageSummary {
  accountCount: number
  requestCount: number
  emptyResponseCount: number
  inputTokens: number
  outputTokens: number
  cachedTokens: number
  reasoningTokens: number
  totalTokens: number
  imageInputTokens: number
  imageOutputTokens: number
  imageRequestCount: number
  imageRequestFailedCount: number
}

export function getUsageSummary() {
  return requestJson<UsageSummary>('/api/admin/usage-stats/summary')
}

export interface AccountUsageStats {
  accountId: string
  email?: string
  label?: string
  planType?: string
  requestCount: number
  emptyResponseCount: number
  inputTokens: number
  outputTokens: number
  cachedTokens: number
  reasoningTokens: number
  totalTokens: number
  imageInputTokens: number
  imageOutputTokens: number
  imageRequestCount: number
  imageRequestFailedCount: number
  lastUsedAt?: string
}

export function getUsageStats() {
  return requestJson<AccountUsageStats[]>('/api/admin/usage-stats')
}
