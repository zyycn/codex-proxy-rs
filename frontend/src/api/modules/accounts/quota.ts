import type { RequestOptions } from '../../request'
import type { Account, AccountIdParam } from './types'
import request from '../../request'

export interface AccountQuotaForecast {
  period: 'weekly' | 'monthly'
  targetDays: number
  extrapolated: boolean
  source: {
    label: string
    usedPercent: number | null
    usedPercentDisplay: string
    observedAt: string | null
    observedAtDisplay: string
    resetAt: string
    tokensDisplay: string
    usdDisplay: string
  } | null
  unavailableReason: string | null
  lowSample: boolean
  incompleteCost: boolean
  incompleteTokens: boolean
  estimatedTokens: number | null
  estimatedTokensDisplay: string
  estimatedUsd: number | null
  estimatedUsdDisplay: string
}

export interface AccountQuotaForecastResponse {
  accountId: string
  generatedAt: string
  forecasts: AccountQuotaForecast[]
}

export interface AccountQuotaResponse {
  account: Account
}

export function getAccountQuotaForecast(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountQuotaForecastResponse>({
    url: '/api/admin/accounts/quota-forecast',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getAccountQuota(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountQuotaResponse>({
    url: '/api/admin/accounts/quota',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function refreshAccountQuota(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountQuotaResponse>({
    url: '/api/admin/accounts/quota/refresh',
    method: 'POST',
    data,
    ...options,
  })
}
