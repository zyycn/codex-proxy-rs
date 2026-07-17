import request from '../request'

export function getApiKeys(data: object) {
  return request({
    url: '/api/admin/keys',
    method: 'GET',
    params: data,
  })
}

export function createApiKey(data: object) {
  return request({
    url: '/api/admin/keys',
    method: 'POST',
    data,
  })
}

export function deleteApiKeys(data: object) {
  return request({
    url: '/api/admin/keys/delete',
    method: 'POST',
    data,
  })
}

export function updateApiKey(data: object) {
  return request({
    url: '/api/admin/keys/update',
    method: 'POST',
    data,
  })
}
