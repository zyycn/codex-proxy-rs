import { requestJson, requestPageJson } from '../request'
import type { PaginatedResult } from '../types'

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
  return requestJson<UsageSummary>('/api/admin/usage/summary')
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
  lastUsedAtDisplay: string
}

export interface UsageStatsQuery {
  page?: number
  pageSize?: number
}

export type UsageStatsPageQuery = UsageStatsQuery & {
  page: number
  pageSize: number
}

export function getUsageStats(
  query: UsageStatsPageQuery,
): Promise<PaginatedResult<AccountUsageStats>>
export function getUsageStats(query?: UsageStatsQuery): Promise<AccountUsageStats[]>
export async function getUsageStats(query: UsageStatsQuery = {}) {
  const params = new URLSearchParams()
  if (query.page) params.set('page', String(query.page))
  if (query.pageSize) params.set('pageSize', String(query.pageSize))
  const url = `/api/admin/usage${params.toString() ? `?${params}` : ''}`

  if (query.page || query.pageSize) {
    return requestPageJson<AccountUsageStats>(url)
  }

  const result = await requestPageJson<AccountUsageStats>(url)
  return result.items
}
