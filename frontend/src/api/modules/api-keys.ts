import request from '../request'

export function getApiKeys(params: any) {
  return request({
    url: '/api/admin/keys',
    method: 'GET',
    params,
  })
}

export function createApiKey(data: any) {
  return request({
    url: '/api/admin/keys',
    method: 'POST',
    data,
  })
}

export function deleteApiKeys(data: any) {
  return request({
    url: '/api/admin/keys/delete',
    method: 'POST',
    data,
  })
}

export function updateApiKey(data: any) {
  return request({
    url: '/api/admin/keys/update',
    method: 'POST',
    data,
  })
}
