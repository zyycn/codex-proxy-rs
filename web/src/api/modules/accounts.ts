import request from '../request'

export function getAccounts(params?: any) {
  return request({
    url: '/api/admin/accounts',
    method: 'GET',
    params,
  })
}

export function createAccount(data: any) {
  return request({
    url: '/api/admin/accounts',
    method: 'POST',
    data,
  })
}

export function deleteAccounts(data: any) {
  return request({
    url: '/api/admin/accounts/delete',
    method: 'POST',
    data,
  })
}

export function refreshAccount(data: any) {
  return request({
    url: '/api/admin/accounts/refresh',
    method: 'POST',
    data,
  })
}

export function updateAccount(data: any) {
  return request({
    url: '/api/admin/accounts/update',
    method: 'POST',
    data,
  })
}

export function getAccountQuota(params: any) {
  return request({
    url: '/api/admin/accounts/quota',
    method: 'GET',
    params,
  })
}

export function testAccountConnection(data: any) {
  return request({
    url: '/api/admin/accounts/health-check',
    method: 'POST',
    data,
  })
}
