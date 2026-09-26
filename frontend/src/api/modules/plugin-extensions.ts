import type { RequestConfig, RequestOptions } from '../request'
import { API_BASE_URL } from '../constants'
import request, { requestRaw } from '../request'

// 管理路由可能执行长任务，HTTP 与页面桥使用同一等待上限。
export const PLUGIN_MANAGEMENT_TIMEOUT_MS = 180_000

const MAXIMUM_MANAGEMENT_BODY_BYTES = 1024 * 1024

export interface PluginManagementTarget {
  instanceId: string
  artifactSha256: string
  revision: number
}

export interface PluginManagementPage {
  id: string
  title: string
  description: string | null
  entry: string
  icon: string | null
}

export interface PluginManagementRoute {
  method: string
  path: string
  requestContentTypes: string[]
  responseContentTypes: string[]
}

export interface PluginManagementResource {
  path: string
  contentType: string
  public: boolean
}

export interface PluginManagementCallback {
  path: string
  responseContentTypes: string[]
}

export interface PluginManagementView {
  target: PluginManagementTarget
  name: string
  configurationSchema: Record<string, unknown>
  pages: PluginManagementPage[]
  routes: PluginManagementRoute[]
  resources: PluginManagementResource[]
  callbacks: PluginManagementCallback[]
}

export interface PluginManagementRawRequest {
  method: string
  path: string
  query?: string
  contentType?: string
  body?: ArrayBuffer
}

export interface PluginManagementCallbackTicket {
  state: string
  expiresAtMs: number
}

export function getPluginManagementViews(options: RequestOptions = {}) {
  return request<PluginManagementView[]>({
    url: '/api/admin/plugins/extensions',
    method: 'GET',
    ...options,
  })
}

export function getPluginManagementResource(
  target: PluginManagementTarget,
  path: string,
  options: RequestOptions = {},
) {
  return requestPluginManagementRaw({
    url: `${pluginManagementTargetPath(target)}/resources/${encodePluginPath(path)}`,
    method: 'GET',
    ...options,
  })
}

export function callPluginManagementRoute(
  target: PluginManagementTarget,
  call: PluginManagementRawRequest,
  options: RequestOptions = {},
) {
  const query = call.query ? `?${call.query}` : ''
  return requestPluginManagementRaw({
    url: `${pluginManagementTargetPath(target)}/api/${encodePluginPath(call.path)}${query}`,
    method: call.method,
    data: call.body?.byteLength ? call.body : undefined,
    headers: call.contentType ? { 'Content-Type': call.contentType } : undefined,
    timeout: PLUGIN_MANAGEMENT_TIMEOUT_MS,
    ...options,
  })
}

export function callPluginModelResponses(
  target: PluginManagementTarget,
  clientKeyId: string,
  body: string,
  signal: AbortSignal,
) {
  const query = new URLSearchParams({ clientKeyId })
  return fetch(`${API_BASE_URL}${pluginManagementTargetPath(target)}/models/responses?${query}`, {
    method: 'POST',
    body,
    cache: 'no-store',
    credentials: 'same-origin',
    headers: {
      'accept': 'application/json, text/event-stream',
      'Content-Type': 'application/json',
    },
    redirect: 'error',
    referrerPolicy: 'no-referrer',
    signal,
  })
}

export function createPluginManagementCallbackTicket(
  target: PluginManagementTarget,
  data: { path: string, ttlSeconds: number },
  options: RequestOptions = {},
) {
  return request<PluginManagementCallbackTicket>({
    url: `${pluginManagementTargetPath(target)}/callback-tickets`,
    method: 'POST',
    data,
    ...options,
  })
}

export function pluginManagementCallbackPath(
  target: PluginManagementTarget,
  path: string,
  state: string,
) {
  return `/plugins/callbacks/${encodeURIComponent(target.instanceId)}/${encodeURIComponent(target.artifactSha256)}/${target.revision}/${encodePluginPath(path)}?state=${encodeURIComponent(state)}`
}

function pluginManagementTargetPath(target: PluginManagementTarget) {
  return `/api/admin/plugins/extensions/${encodeURIComponent(target.instanceId)}/${encodeURIComponent(target.artifactSha256)}/${target.revision}`
}

function encodePluginPath(path: string) {
  return path.split('/').map(segment => encodeURIComponent(segment)).join('/')
}

function requestPluginManagementRaw(config: RequestConfig) {
  return requestRaw(config, {
    maximumBytes: MAXIMUM_MANAGEMENT_BODY_BYTES,
    accept: ({ header }) => {
      const policy = header('content-security-policy')
      return policy?.split(';').some(directive => directive.trim() === 'sandbox allow-scripts') === true
        && header('x-content-type-options')?.toLowerCase() === 'nosniff'
    },
  })
}
