import request from '../request'

export function getAccounts(data: object) {
  return request({
    url: '/api/admin/accounts',
    method: 'GET',
    params: data,
  })
}

export function importAccounts(data: object) {
  return request({
    url: '/api/admin/accounts/import',
    method: 'POST',
    data,
  })
}

export function exportAccounts(data: object) {
  return request({
    url: '/api/admin/accounts/export',
    method: 'GET',
    params: data,
  })
}

export function authorizeAccountOAuth(data?: object) {
  return request({
    url: '/api/admin/accounts/oauth/authorize',
    method: 'POST',
    data,
  })
}

export function exchangeAccountOAuth(data: object) {
  return request({
    url: '/api/admin/accounts/oauth/exchange',
    method: 'POST',
    data,
  })
}

export function deleteAccounts(data: object) {
  return request({
    url: '/api/admin/accounts/delete',
    method: 'POST',
    data,
  })
}

export function refreshAccount(data: object) {
  return request({
    url: '/api/admin/accounts/refresh',
    method: 'POST',
    data,
  })
}

export function updateAccount(data: object) {
  return request({
    url: '/api/admin/accounts/update',
    method: 'POST',
    data,
  })
}

export function getAccountQuota(data: object) {
  return request({
    url: '/api/admin/accounts/quota',
    method: 'GET',
    params: data,
  })
}

export function getAccountModels(data: object) {
  return request({
    url: '/api/admin/accounts/models',
    method: 'GET',
    params: data,
  })
}

export function refreshAccountModels(data: object) {
  return request({
    url: '/api/admin/accounts/models',
    method: 'POST',
    data,
  })
}
