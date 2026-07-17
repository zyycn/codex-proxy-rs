import { z } from 'zod'

import { requestParsed } from '../request'

const _loginPayloadSchema = z.object({
  username: z.string().optional(),
  password: z.string(),
})

const loginResultSchema = z.object({
  expiresAt: z.string(),
})

const authStatusSchema = z.object({
  authenticated: z.boolean(),
})

const logoutResultSchema = z.object({
  message: z.string(),
})

export function login(data: z.input<typeof _loginPayloadSchema>) {
  return requestParsed({
    url: '/api/admin/login',
    method: 'POST',
    data,
  }, loginResultSchema)
}

export function getAuthStatus() {
  return requestParsed({
    url: '/api/admin/auth/status',
    method: 'GET',
  }, authStatusSchema)
}

export function logout() {
  return requestParsed({
    url: '/api/admin/logout',
    method: 'POST',
  }, logoutResultSchema)
}
