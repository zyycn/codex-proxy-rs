import request from '../request'

export interface OutboundProxy {
  id: string
  name: string
  /** 已脱敏 endpoint；不含用户名与密码 */
  endpoint: string
  accountCount: number
  createdAt: string
  updatedAt: string
}

export interface OutboundProxyPageMeta {
  page: number
  pageSize: number
  total: number
  totalPages: number
}

export interface OutboundProxyListResponse {
  items: OutboundProxy[]
  page: OutboundProxyPageMeta
  configRevision: number
}

export interface OutboundProxyMutationResponse {
  id: string
  record: OutboundProxy | null
  configRevision: number
}

export interface RevealedOutboundProxy {
  id: string
  url: string
}

export interface OutboundProxyTestResult {
  success: boolean
  latencyMs: number
  statusCode: number | null
  targetUrl: string
  error: string | null
}

interface OutboundProxyListParams {
  page: number
  pageSize: number
  search?: string
}

interface OutboundProxyCreateParam {
  name: string
  url: string
}

interface OutboundProxyUpdateParam {
  id: string
  name: string
  /** 留空保留当前 URL */
  url?: string
}

interface OutboundProxyIdParam {
  id: string
}

interface OutboundProxyTestParam {
  id?: string
  url?: string
  targetUrl?: string
}

export function getOutboundProxies(data: OutboundProxyListParams) {
  return request<OutboundProxyListResponse>({
    url: '/api/admin/proxies',
    method: 'GET',
    params: data,
  })
}

export function createOutboundProxy(data: OutboundProxyCreateParam) {
  return request<OutboundProxyMutationResponse>({
    url: '/api/admin/proxies/create',
    method: 'POST',
    data,
  })
}

export function updateOutboundProxy(data: OutboundProxyUpdateParam) {
  return request<OutboundProxyMutationResponse>({
    url: '/api/admin/proxies/update',
    method: 'POST',
    data,
  })
}

export function deleteOutboundProxy(data: OutboundProxyIdParam) {
  return request<OutboundProxyMutationResponse>({
    url: '/api/admin/proxies/delete',
    method: 'POST',
    data,
  })
}

export function revealOutboundProxy(data: OutboundProxyIdParam) {
  return request<RevealedOutboundProxy>({
    url: '/api/admin/proxies/reveal',
    method: 'GET',
    params: data,
  })
}

export function testOutboundProxy(data: OutboundProxyTestParam) {
  return request<OutboundProxyTestResult>({
    url: '/api/admin/proxies/test',
    method: 'POST',
    data,
  })
}
