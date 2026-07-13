import request from '../request'

export function getUsageRecords(params?: any) {
  return request({
    url: '/api/admin/usage/records',
    method: 'GET',
    params,
  })
}

export function getOpsErrors(params?: any) {
  return request({
    url: '/api/admin/ops/errors',
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

export function getUsageRecordInsightsOverview(params?: any) {
  return request({
    url: '/api/admin/usage/records/insights/overview',
    method: 'GET',
    params,
  })
}

export function getUsageRecordInsightsDiagnostics(params: any) {
  return request({
    url: '/api/admin/usage/records/insights/diagnostics',
    method: 'GET',
    params,
  })
}
