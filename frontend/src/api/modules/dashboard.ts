import request from '../request'

export function getDashboardSummary(params?: any) {
  return request({
    url: '/api/admin/dashboard/summary',
    method: 'GET',
    params,
  })
}

export function getDashboardTrend(params: any) {
  return request({
    url: '/api/admin/dashboard/trend',
    method: 'GET',
    params,
  })
}
