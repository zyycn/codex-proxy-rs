import request from '../request'

export interface AuthSession {
  role: 'admin' | 'key'
  expiresAt: string
}

export type LoginParam
  = | { mode: 'admin', username: string, password: string }
    | { mode: 'key', apiKey: string }

export interface AuthStatusResponse {
  authenticated: boolean
  session: AuthSession | null
}

export interface LogoutResponse {
  message: string
}

export function login(data: LoginParam) {
  return request<AuthSession>({
    url: '/api/auth/login',
    method: 'POST',
    data,
  })
}

export function getAuthStatus() {
  return request<AuthStatusResponse>({
    url: '/api/auth/status',
    method: 'GET',
  })
}

export function logout() {
  return request<LogoutResponse>({
    url: '/api/auth/logout',
    method: 'POST',
  })
}
