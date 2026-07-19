import request from '../request'

export function getApiKeys(data: object) {
  return request({
    url: '/api/admin/client-keys',
    method: 'GET',
    params: data,
  })
}

export function createApiKey(data: object) {
  return request({
    url: '/api/admin/client-keys',
    method: 'POST',
    data,
  })
}

export function revealApiKey(data: object) {
  return request({
    url: '/api/admin/client-keys/reveal',
    method: 'GET',
    params: data,
  })
}

export function deleteApiKey(data: object) {
  return request({
    url: '/api/admin/client-keys/delete',
    method: 'POST',
    data,
  })
}

export function disableApiKey(data: object) {
  return request({
    url: '/api/admin/client-keys/disable',
    method: 'POST',
    data,
  })
}

export function enableApiKey(data: object) {
  return request({
    url: '/api/admin/client-keys/enable',
    method: 'POST',
    data,
  })
}
