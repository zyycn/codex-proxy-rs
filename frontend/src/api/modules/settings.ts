import request from '../request'

export function getSettings() {
  return request({
    url: '/api/admin/settings',
    method: 'GET',
  })
}

export function updateSettings(data: object) {
  return request({
    url: '/api/admin/settings/update',
    method: 'POST',
    data,
  })
}

export function getAdminApiKeyStatus() {
  return request({
    url: '/api/admin/settings/admin-api-key',
    method: 'GET',
  })
}

export function regenerateAdminApiKey() {
  return request({
    url: '/api/admin/settings/admin-api-key/regenerate',
    method: 'POST',
  })
}

export function deleteAdminApiKey() {
  return request({
    url: '/api/admin/settings/admin-api-key/delete',
    method: 'POST',
  })
}
