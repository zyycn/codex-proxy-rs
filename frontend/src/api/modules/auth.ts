import type { RequestOptions } from '../request'
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

export function login(data: LoginParam, options: RequestOptions = {}) {
  return request<LoginResponse>({
    url: '/api/admin/auth/login',
    method: 'POST',
    data,
    ...options,
  })
}

export function getAuthStatus(options: RequestOptions = {}) {
  return request<AuthStatusResponse>({
    url: '/api/admin/auth/status',
    method: 'GET',
    ...options,
  })
}

export function logout(options: RequestOptions = {}) {
  return request<LogoutResponse>({
    url: '/api/admin/auth/logout',
    method: 'POST',
    ...options,
  })
}
