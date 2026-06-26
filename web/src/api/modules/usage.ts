import request from '../request'

export function getUsageSummary() {
  return request({
    url: '/api/admin/usage/summary',
    method: 'GET',
  })
}

export function getUsageStats(params?: any) {
  return request({
    url: '/api/admin/usage',
    method: 'GET',
    params,
  })
}
