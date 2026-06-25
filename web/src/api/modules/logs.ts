import { requestJson, requestPageJson } from '../request'
import type { PaginatedResult } from '../types'

export interface EventLog {
  id: string
  requestId?: string
  kind: string
  level: 'debug' | 'info' | 'warn' | 'error'
  accountId?: string
  route?: string
  model?: string
  statusCode?: number
  transport?: string
  attemptIndex?: number
  upstreamStatusCode?: number
  failureClass?: string
  responseId?: string
  upstreamRequestId?: string
  latencyMs?: number
  message: string
  metadata?: any
  createdAt: string
  createdAtDisplay: string
}

export interface LogsQuery {
  page?: number
  pageSize?: number
  level?: EventLog['level']
  kind?: string
  accountId?: string
  requestId?: string
  route?: string
  model?: string
  statusCode?: number
  transport?: string
  search?: string
}

export type LogsPageQuery = LogsQuery & {
  page: number
  pageSize: number
}

export function getLogs(query: LogsPageQuery): Promise<PaginatedResult<EventLog>>
export function getLogs(query?: LogsQuery): Promise<EventLog[]>
export async function getLogs(query: LogsQuery = {}) {
  const params = new URLSearchParams()
  if (query.page) params.set('page', String(query.page))
  if (query.pageSize) params.set('pageSize', String(query.pageSize))
  if (query.level) params.set('level', query.level)
  if (query.kind) params.set('kind', query.kind)
  if (query.accountId) params.set('accountId', query.accountId)
  if (query.requestId) params.set('requestId', query.requestId)
  if (query.route) params.set('route', query.route)
  if (query.model) params.set('model', query.model)
  if (query.statusCode) params.set('statusCode', String(query.statusCode))
  if (query.transport) params.set('transport', query.transport)
  if (query.search) params.set('search', query.search)

  const url = `/api/admin/logs${params.toString() ? `?${params}` : ''}`
  if (query.page || query.pageSize) {
    return requestPageJson<EventLog>(url)
  }

  const result = await requestPageJson<EventLog>(url)
  return result.items
}

export function getLogDetail(logId: string) {
  const params = new URLSearchParams({ id: logId })
  return requestJson<EventLog>(`/api/admin/logs/detail?${params}`)
}

export function clearLogs() {
  return requestJson<{ cleared: number }>('/api/admin/logs/delete', {
    method: 'POST',
  })
}
