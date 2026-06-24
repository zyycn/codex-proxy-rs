import { requestJson } from '../request'

// ==================== 认证相关 ====================

export interface AuthStatus {
  authenticated: boolean
}

export interface LoginRequest {
  username?: string
  password: string
}

export interface LoginResponse {
  expiresAt: string
}

export function login(payload: LoginRequest) {
  return requestJson<LoginResponse>('/api/admin/login', {
    method: 'POST',
    data: payload,
  })
}

export function getAuthStatus() {
  return requestJson<AuthStatus>('/api/admin/auth/status')
}

export function logout() {
  return requestJson<void>('/api/admin/logout', { method: 'POST' })
}
