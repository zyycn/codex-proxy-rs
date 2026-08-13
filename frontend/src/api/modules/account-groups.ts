import request from '../request'

export interface AccountGroupRef {
  id: string
  name: string
  color: string
  enabled: boolean
}

export interface AccountGroupAccountSummary {
  available: number
  limited: number
  total: number
}

export interface AccountGroupCapacity {
  usedSlots: number | null
  totalSlots: number
}

export interface AccountGroupUsage {
  todayUsd: string
  totalUsd: string
}

export interface AccountGroup extends AccountGroupRef {
  description: string | null
  memberCount: number
  providerCounts: Record<string, number>
  clientKeyCount: number
  accountSummary: AccountGroupAccountSummary
  capacity: AccountGroupCapacity
  usage: AccountGroupUsage
  createdAt: string
  updatedAt: string
}

export interface AccountGroupPageMeta {
  page: number
  pageSize: number
  total: number
  totalPages: number
}

export interface AccountGroupListResponse {
  items: AccountGroup[]
  page: AccountGroupPageMeta
  configRevision: number
}

export interface AccountGroupMutationResponse {
  id: string
  record: AccountGroup | null
  configRevision: number
}

interface AccountGroupListParams {
  page: number
  pageSize: number
  search?: string
  enabled?: boolean
}

interface AccountGroupCreateParam {
  name: string
  description: string | null
  color: string
}

interface AccountGroupUpdateParam {
  id: string
  name: string
  description: string | null
  color: string
}

interface AccountGroupIdParam {
  id: string
}

export function getAccountGroups(data: AccountGroupListParams) {
  return request<AccountGroupListResponse>({
    url: '/api/admin/account-groups',
    method: 'GET',
    params: data,
  })
}

export function createAccountGroup(data: AccountGroupCreateParam) {
  return request<AccountGroupMutationResponse>({
    url: '/api/admin/account-groups/create',
    method: 'POST',
    data,
  })
}

export function updateAccountGroup(data: AccountGroupUpdateParam) {
  return request<AccountGroupMutationResponse>({
    url: '/api/admin/account-groups/update',
    method: 'POST',
    data,
  })
}

export function enableAccountGroup(data: AccountGroupIdParam) {
  return request<AccountGroupMutationResponse>({
    url: '/api/admin/account-groups/enable',
    method: 'POST',
    data,
  })
}

export function disableAccountGroup(data: AccountGroupIdParam) {
  return request<AccountGroupMutationResponse>({
    url: '/api/admin/account-groups/disable',
    method: 'POST',
    data,
  })
}

export function deleteAccountGroup(data: AccountGroupIdParam) {
  return request<AccountGroupMutationResponse>({
    url: '/api/admin/account-groups/delete',
    method: 'POST',
    data,
  })
}
