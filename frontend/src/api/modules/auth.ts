import request from '../request'

export interface LoginResponse {
  expiresAt: string
}

export interface AuthStatusResponse {
  authenticated: boolean
}

export interface LogoutResponse {
  message: string
}

interface LoginParam {
  username: string
  password: string
}

export function login(data: LoginParam) {
  return request<LoginResponse>({
    url: '/api/admin/auth/login',
    method: 'POST',
    data,
  })
}

export function getAuthStatus() {
  return request<AuthStatusResponse>({
    url: '/api/admin/auth/status',
    method: 'GET',
  })
}

export function logout() {
  return request<LogoutResponse>({
    url: '/api/admin/auth/logout',
    method: 'POST',
  })
}
