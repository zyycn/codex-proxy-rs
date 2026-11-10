import type { RequestOptions } from '../../request'
import type { AccountIdParam } from './types'
import request from '../../request'

export interface AccountResetCredit {
  id: string
  status: string | null
  title: string | null
  expiresAt: string | null
  expiresAtDisplay: string | null
  resetType: string | null
}

export interface AccountResetCreditsResponse {
  availableCount: number
  credits: AccountResetCredit[]
}

export interface AccountResetCreditResultResponse {
  code: string
  credit: AccountResetCredit | null
}

export interface AccountResetCreditAutomation {
  enabled: boolean
  weeklyTriggerRemainingPercent: number | null
  leadMinutes: number
  nextDiscoveryAtDisplay: string | null
  nextConsumeAtDisplay: string | null
  lastResult: string | null
}

interface AccountResetCreditConsumeParam extends AccountIdParam {
  creditId?: string
  redeemRequestId: string
}

interface AccountResetCreditAutomationParam extends AccountIdParam {
  enabled: boolean
}

export function getAccountResetCredits(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountResetCreditsResponse>({
    url: '/api/admin/accounts/reset-credits',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function consumeAccountResetCredit(data: AccountResetCreditConsumeParam, options: RequestOptions = {}) {
  return request<AccountResetCreditResultResponse>({
    url: '/api/admin/accounts/reset-credits',
    method: 'POST',
    data,
    ...options,
  })
}

export function getAccountResetCreditAutomation(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountResetCreditAutomation>({
    url: '/api/admin/accounts/reset-credits/automation',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function setAccountResetCreditAutomation(data: AccountResetCreditAutomationParam, options: RequestOptions = {}) {
  return request<AccountResetCreditAutomation>({
    url: '/api/admin/accounts/reset-credits/automation',
    method: 'POST',
    data,
    ...options,
  })
}
