import type { RequestOptions } from '../request'
import type { AccountGroupRef } from './account-groups'
import request from '../request'

export interface OutboundProxyTest {
  success: boolean
  latencyMs: number
  exitIp: string | null
  message: string
}

export interface OutboundProxyRecord {
  id: string
  name: string
  endpoint: string
  hasAuthentication: boolean
  revision: number
  accountCount: number
  lastTestAt: string | null
  lastTest: OutboundProxyTest | null
  createdAt: string
  updatedAt: string
}

interface ProxyPage {
  items: OutboundProxyRecord[]
  page: { page: number, pageSize: number, total: number, totalPages: number }
}

export interface OutboundProxyAccount {
  id: string
  name: string
  email: string | null
  provider: string
  authenticationKind: string
  planType: string | null
  planTypeDisplay: string | null
  groups: AccountGroupRef[]
  enabled: boolean
}

interface ProxyAccountPage {
  items: OutboundProxyAccount[]
  page: ProxyPage['page']
}

export function getProxyAccounts(params: { proxyId: string, page: number, pageSize: number, search?: string }, options: RequestOptions = {}) {
  return request<ProxyAccountPage>({
    url: '/api/admin/proxies/accounts',
    method: 'GET',
    params,
    ...options,
  })
}

export function removeProxyAccount(data: { proxyId: string, accountId: string }, options: RequestOptions = {}) {
  return request<{ configRevision: number }>({
    url: '/api/admin/proxies/accounts/remove',
    method: 'POST',
    data,
    ...options,
  })
}

interface ProxyMutation {
  record: OutboundProxyRecord
  configRevision: number
}

export function getProxies(params: { page: number, pageSize: number, search?: string }, options: RequestOptions = {}) {
  return request<ProxyPage>({
    url: '/api/admin/proxies',
    method: 'GET',
    params,
    ...options,
  })
}

export function createProxy(data: { name: string, proxyUrl: string }, options: RequestOptions = {}) {
  return request<ProxyMutation>({
    url: '/api/admin/proxies/create',
    method: 'POST',
    data,
    ...options,
  })
}

export function updateProxy(data: { id: string, revision: number, name: string, proxyUrl?: string }, options: RequestOptions = {}) {
  return request<ProxyMutation>({
    url: '/api/admin/proxies/update',
    method: 'POST',
    data,
    ...options,
  })
}

export function deleteProxy(data: { id: string, revision: number }, options: RequestOptions = {}) {
  return request<{ configRevision: number }>({
    url: '/api/admin/proxies/delete',
    method: 'POST',
    data,
    ...options,
  })
}

export function testProxy(data: { id: string, revision: number }, options: RequestOptions = {}) {
  return request<OutboundProxyRecord>({
    url: '/api/admin/proxies/test',
    method: 'POST',
    data,
    timeout: 25000,
    ...options,
  })
}

export function probeProxy(data: { proxyUrl: string }, options: RequestOptions = {}) {
  return request<OutboundProxyTest>({
    url: '/api/admin/proxies/probe',
    method: 'POST',
    data,
    timeout: 25000,
    ...options,
  })
}
