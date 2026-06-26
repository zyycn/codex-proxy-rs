import request from '../request'

export function getSettings() {
  return request({
    url: '/api/admin/settings',
    method: 'GET',
  })
}

export function updateSettings(data: any) {
  return request({
    url: '/api/admin/settings',
    method: 'POST',
    data,
  })
}
