import request from '../request'

export function login(data: object) {
  return request({
    url: '/api/admin/login',
    method: 'POST',
    data,
  })
}

export function getAuthStatus() {
  return request({
    url: '/api/admin/auth/status',
    method: 'GET',
  })
}

export function logout() {
  return request({
    url: '/api/admin/logout',
    method: 'POST',
  })
}
