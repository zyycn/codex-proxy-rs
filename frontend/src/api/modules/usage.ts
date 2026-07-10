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

export function getUsageRecordModelDistribution(params?: any) {
  return request({
    url: '/api/admin/usage/records/insights/models',
    method: 'GET',
    params,
  })
}

export function getUsageRecordEndpointDistribution(params?: any) {
  return request({
    url: '/api/admin/usage/records/insights/endpoints',
    method: 'GET',
    params,
  })
}

export function getUsageRecordTokenTrend(params?: any) {
  return request({
    url: '/api/admin/usage/records/insights/token-trend',
    method: 'GET',
    params,
  })
}

export function getUsageRecordLatencyTrend(params?: any) {
  return request({
    url: '/api/admin/usage/records/insights/latency-trend',
    method: 'GET',
    params,
  })
}
