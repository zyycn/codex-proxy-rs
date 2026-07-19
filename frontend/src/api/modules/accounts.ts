import request from '../request'

export function getAccounts(data: object) {
  return request({
    url: '/api/admin/accounts',
    method: 'GET',
    params: data,
  })
}

export function getProviderInstances(data: object) {
  return request({
    url: '/api/admin/provider-instances',
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

export function importCodexCredentialsDocument(data: object) {
  return request({
    url: '/api/admin/openai/credentials/import-document',
    method: 'POST',
    data,
  })
}

export function enableCodexCredential(data: object) {
  return request({
    url: '/api/admin/openai/credentials/enable',
    method: 'POST',
    data,
  })
}

export function disableCodexCredential(data: object) {
  return request({
    url: '/api/admin/openai/credentials/disable',
    method: 'POST',
    data,
  })
}

export function deleteCodexCredential(data: object) {
  return request({
    url: '/api/admin/openai/credentials/delete',
    method: 'POST',
    data,
  })
}

export function startCodexOAuthAuthorization(data: object) {
  return request({
    url: '/api/admin/openai/oauth/authorization/start',
    method: 'POST',
    data,
  })
}

export function completeCodexOAuthAuthorization(data: object) {
  return request({
    url: '/api/admin/openai/oauth/authorization/complete',
    method: 'POST',
    data,
  })
}

export function importXaiCredentialsDocument(data: object) {
  return request({
    url: '/api/admin/xai/credentials/import-document',
    method: 'POST',
    data,
  })
}

export function startXaiOAuthAuthorization(data: object) {
  return request({
    url: '/api/admin/xai/oauth/authorization/start',
    method: 'POST',
    data,
  })
}

export function completeXaiOAuthAuthorization(data: object) {
  return request({
    url: '/api/admin/xai/oauth/authorization/complete',
    method: 'POST',
    data,
  })
}

export function enableXaiCredential(data: object) {
  return request({
    url: '/api/admin/xai/credentials/enable',
    method: 'POST',
    data,
  })
}

export function disableXaiCredential(data: object) {
  return request({
    url: '/api/admin/xai/credentials/disable',
    method: 'POST',
    data,
  })
}

export function deleteXaiCredential(data: object) {
  return request({
    url: '/api/admin/xai/credentials/delete',
    method: 'POST',
    data,
  })
}
