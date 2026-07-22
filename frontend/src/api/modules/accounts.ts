import request from '../request'

export function getAccounts(data: object) {
  return request({
    url: '/api/admin/accounts',
    method: 'GET',
    params: data,
  })
}

export function exportAccounts(data: object) {
  return request({
    url: '/api/admin/accounts/export',
    method: 'GET',
    params: data,
  })
}

export function refreshAccount(data: object) {
  return request({
    url: '/api/admin/accounts/refresh',
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

export function refreshAccountQuota(data: object) {
  return request({
    url: '/api/admin/accounts/quota/refresh',
    method: 'POST',
    data,
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
    url: '/api/admin/accounts/models/refresh',
    method: 'POST',
    data,
  })
}

export function importAccounts(data: object) {
  return request({
    url: '/api/admin/accounts/import',
    method: 'POST',
    data,
  })
}

export function enableAccount(data: object) {
  return request({
    url: '/api/admin/accounts/enable',
    method: 'POST',
    data,
  })
}

export function disableAccount(data: object) {
  return request({
    url: '/api/admin/accounts/disable',
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

export function startAccountOAuth(data: object) {
  return request({
    url: '/api/admin/accounts/oauth/start',
    method: 'POST',
    data,
  })
}

export function completeAccountOAuth(data: object) {
  return request({
    url: '/api/admin/accounts/oauth/complete',
    method: 'POST',
    data,
  })
}
