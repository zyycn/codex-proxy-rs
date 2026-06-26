import request from '../request'

export function getDashboardSummary() {
  return request({
    url: '/api/admin/dashboard/summary',
    method: 'GET',
  })
}

export function getDashboardTrend(params: any) {
  return request({
    url: '/api/admin/dashboard/trend',
    method: 'GET',
    params,
  })
}
