import request from '../request'

export function getUsageRecords(params?: any) {
  return request({
    url: '/api/admin/usage/records',
    method: 'GET',
    params,
  })
}

export function getUsageRecordDetail(params: any) {
  return request({
    url: '/api/admin/usage/records/detail',
    method: 'GET',
    params,
  })
}

export function getUsageRecordSummary(params?: any) {
  return request({
    url: '/api/admin/usage/records/summary',
    method: 'GET',
    params,
  })
}

export function getUsageRecordInsights(params?: any) {
  return request({
    url: '/api/admin/usage/records/insights',
    method: 'GET',
    params,
  })
}
