import request, { type ApiPayload } from '../request'

// ==================== API Keys 管理 ====================

export function getApiKeys() {
  return request({
    url: '/api/admin/keys',
    method: 'GET',
  })
}

export function createApiKey(data: ApiPayload) {
  return request({
    url: '/api/admin/keys',
    method: 'POST',
    data,
  })
}

export function deleteApiKeys(data: ApiPayload) {
  return request({
    url: '/api/admin/keys/delete',
    method: 'POST',
    data,
  })
}

export function updateApiKey(data: ApiPayload) {
  return request({
    url: '/api/admin/keys/update',
    method: 'POST',
    data,
  })
}
