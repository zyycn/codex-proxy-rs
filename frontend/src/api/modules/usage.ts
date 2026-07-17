import request from '../request'

export function getUsageRecords(data: object) {
  return request({
    url: '/api/admin/usage/records',
    method: 'GET',
    params: data,
  })
}

export function getOpsErrors(data: object) {
  return request({
    url: '/api/admin/ops/errors',
    method: 'GET',
    params: data,
  })
}

export function getUsageRecordDetail(data: object) {
  return request({
    url: '/api/admin/usage/records/detail',
    method: 'GET',
    params: data,
  })
}

export function getUsageRecordSummary(data: object) {
  return request({
    url: '/api/admin/usage/records/summary',
    method: 'GET',
    params: data,
  })
}

export function getUsageRecordInsightsOverview(data: object) {
  return request({
    url: '/api/admin/usage/records/insights/overview',
    method: 'GET',
    params: data,
  })
}

export function getUsageRecordInsightsDiagnostics(data: object) {
  return request({
    url: '/api/admin/usage/records/insights/diagnostics',
    method: 'GET',
    params: data,
  })
}
