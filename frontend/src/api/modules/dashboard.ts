import request from '../request'

export function getDashboardSummary(data: object) {
  return request({
    url: '/api/admin/dashboard/summary',
    method: 'GET',
    params: data,
  })
}

export function getDashboardTrend(data: object) {
  return request({
    url: '/api/admin/dashboard/trend',
    method: 'GET',
    params: data,
  })
}
