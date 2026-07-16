import request from '../request'

const ACCOUNT_EXPORT_CONFIRMATION = 'export_sensitive_accounts'

export function getAccounts(params?: any) {
  return request({
    url: '/api/admin/accounts',
    method: 'GET',
    params,
  })
}

export function importAccounts(data: any) {
  return request({
    url: '/api/admin/accounts/import',
    method: 'POST',
    data,
  })
}

export function exportAccounts(params?: any) {
  return request({
    url: '/api/admin/accounts/export',
    method: 'GET',
    params: {
      ...params,
      confirm: ACCOUNT_EXPORT_CONFIRMATION,
    },
  })
}

export function authorizeAccountOAuth(data = {}) {
  return request({
    url: '/api/admin/accounts/oauth/authorize',
    method: 'POST',
    data,
  })
}

export function exchangeAccountOAuth(data: any) {
  return request({
    url: '/api/admin/accounts/oauth/exchange',
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

export function getAccountModels(data: any) {
  return request({
    url: '/api/admin/accounts/models',
    method: 'GET',
    params: {
      id: data.id,
    },
  })
}

export function refreshAccountModels(data: any) {
  return request({
    url: '/api/admin/accounts/models',
    method: 'POST',
    data,
  })
}
