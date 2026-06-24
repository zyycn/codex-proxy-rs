import { requestJson } from '../request'

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
}

export interface LogsQuery {
  limit?: number
  cursor?: string
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

export function getLogs(query: LogsQuery = {}) {
  const params = new URLSearchParams()
  if (query.limit) params.set('limit', String(query.limit))
  if (query.cursor) params.set('cursor', query.cursor)
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
  return requestJson<EventLog[]>(url)
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
