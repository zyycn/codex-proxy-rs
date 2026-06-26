import request from '../request'

export function getLogs(params?: any) {
  return request({
    url: '/api/admin/logs',
    method: 'GET',
    params,
  })
}

export function getLogDetail(params: any) {
  return request({
    url: '/api/admin/logs/detail',
    method: 'GET',
    params,
  })
}

export function clearLogs() {
  return request({
    url: '/api/admin/logs/delete',
    method: 'POST',
  })
}
