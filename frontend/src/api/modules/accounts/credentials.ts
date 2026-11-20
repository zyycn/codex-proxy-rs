import type { RequestOptions } from '../../request'
import type { AccountIdParam, AccountModelAccess, AccountRefreshResponse } from './types'
import request from '../../request'

export interface AccountImportResponse {
  importedCount: number
  accountIds: string[]
}

export type ImportItemStatus = 'pending' | 'running' | 'succeeded' | 'failed' | 'unknown' | 'skipped'

export interface AccountImportTask {
  taskId: string
  createdAt: string
  createdAtDisplay: string
  finishedAt: string | null
  finishedAtDisplay: string | null
  stopRequested: boolean
  total: number
  counts: Record<ImportItemStatus, number> & { importedAccounts: number }
}

export interface AccountImportTaskItem {
  index: number
  provider: string
  status: ImportItemStatus
  accountIds: string[]
  message: string | null
}

export interface AccountImportTaskDetail extends AccountImportTask {
  items: AccountImportTaskItem[]
}

export interface AccountOAuthCompleteResponse {
  accountId: string
}

export interface AccountOAuthStartResponse {
  flowId: string
  authorizationUrl: string
  expiresAt: string
}

export interface AccountImportSettings {
  name?: string
  notes?: string
  enabled: boolean
  concurrencyLimit: number | null
  weight: number
  modelAccess?: AccountModelAccess
  groupIds: string[]
}

export interface AccountImportParam {
  outboundProxyId?: string
  settings?: AccountImportSettings
  provider: string
  data: unknown
}

interface AccountImportTaskIdParam {
  taskId: string
}

interface CreateAccountImportTaskParam {
  submissionId: string
  items: AccountImportParam[]
}

interface AccountOAuthStartParam {
  outboundProxyId?: string
  provider: string
  name: string
  accountId?: string
}

interface AccountOAuthCompleteParam {
  settings?: AccountImportSettings
  provider: string
  flowId: string
  callbackUrl: string
}

interface AccountExportParam {
  accountIds: string
  confirm: string
}

interface AccountExportResponse {
  exportedAt: string
  fileName: string
  documents: unknown[]
}

interface AccountImportTaskListResponse {
  items: AccountImportTask[]
}

export function exportAccounts(data: AccountExportParam) {
  return request<AccountExportResponse>({
    url: '/api/admin/accounts/export',
    method: 'GET',
    params: data,
  })
}

export function refreshAccount(data: AccountIdParam) {
  return request<AccountRefreshResponse>({
    url: '/api/admin/accounts/refresh',
    method: 'POST',
    data,
  })
}

export function importAccounts(data: AccountImportParam, options: RequestOptions = {}) {
  return request<AccountImportResponse>({
    url: '/api/admin/accounts/import',
    method: 'POST',
    ...providerInputBody(data),
    ...options,
  })
}

export function createAccountImportTask(data: CreateAccountImportTaskParam) {
  return request<AccountImportTask>({
    url: '/api/admin/accounts/import-tasks',
    method: 'POST',
    ...providerInputBody(data),
  })
}

export function getAccountImportTasks(options: RequestOptions = {}) {
  return request<AccountImportTaskListResponse>({
    url: '/api/admin/accounts/import-tasks',
    method: 'GET',
    ...options,
  })
}

export function getAccountImportTask(data: AccountImportTaskIdParam, options: RequestOptions = {}) {
  return request<AccountImportTaskDetail>({
    url: '/api/admin/accounts/import-tasks/detail',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function stopAccountImportTask(data: AccountImportTaskIdParam) {
  return request<AccountImportTaskDetail>({
    url: '/api/admin/accounts/import-tasks/stop',
    method: 'POST',
    data,
  })
}

export function startAccountOAuth(data: AccountOAuthStartParam, options: RequestOptions = {}) {
  return request<AccountOAuthStartResponse>({
    url: '/api/admin/accounts/oauth/start',
    method: 'POST',
    ...providerInputBody(data),
    ...options,
  })
}

function providerInputBody(data: object) {
  // Provider 文档是不透明 JSON，先编码，避免 HTTP 客户端合并配置时过滤特殊字段名。
  return {
    headers: { 'Content-Type': 'application/json' },
    data: JSON.stringify(data),
  }
}

export function completeAccountOAuth(data: AccountOAuthCompleteParam, options: RequestOptions = {}) {
  return request<AccountOAuthCompleteResponse>({
    url: '/api/admin/accounts/oauth/complete',
    method: 'POST',
    data,
    ...options,
  })
}
