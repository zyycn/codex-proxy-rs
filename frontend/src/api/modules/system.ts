import request from '../request'

export function getSystemVersion(timeout = 0) {
  return request({
    url: '/api/admin/system/version',
    method: 'GET',
    ...(timeout ? { timeout } : {}),
  })
}

export function getSystemUpdateDetail(data: object) {
  return request({
    url: '/api/admin/system/update-detail',
    method: 'GET',
    params: data,
  })
}

export function performSystemUpdate(data: object) {
  return request({
    url: '/api/admin/system/update',
    method: 'POST',
    data,
  })
}

export function restartSystem() {
  return request({
    url: '/api/admin/system/restart',
    method: 'POST',
  })
}
