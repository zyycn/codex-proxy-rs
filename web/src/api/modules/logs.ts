import { requestJson } from '../request'

export interface EventLog {
  id: string
  requestId?: string
  kind: string
  level: 'info' | 'warning' | 'error'
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
  before?: string
  level?: string
  kind?: string
  accountId?: string
  route?: string
}

export function getLogs(query: LogsQuery = {}) {
  const params = new URLSearchParams()
  if (query.limit) params.set('limit', String(query.limit))
  if (query.before) params.set('before', query.before)
  if (query.level) params.set('level', query.level)
  if (query.kind) params.set('kind', query.kind)
  if (query.accountId) params.set('account_id', query.accountId)
  if (query.route) params.set('route', query.route)

  const url = `/api/admin/logs${params.toString() ? `?${params}` : ''}`
  return requestJson<EventLog[]>(url)
}

export function getLogDetail(logId: string) {
  return requestJson<EventLog>(`/api/admin/logs/${logId}`)
}

export function clearLogs() {
  return requestJson<{ deleted: number }>('/api/admin/logs', {
    method: 'DELETE',
  })
}
