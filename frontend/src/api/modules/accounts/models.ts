import type { RequestOptions } from '../../request'
import type { AccountIdParam, AccountModelAccess } from './types'
import request from '../../request'

export interface AccountModelsResponse {
  models: Array<{ id: string, label: string }>
  observedAt?: string | null
  observedAtDisplay?: string
  /** 强制刷新成功后，服务端实际保留的账号白名单；普通查询不返回。 */
  modelAccess?: AccountModelAccess
}

export interface AccountModelCatalogResponse {
  modelCount: number
  observedAt: string
  /** Codex `model_catalog_json` 的文件正文，原样落盘即可被客户端加载。 */
  catalog: unknown
}

export function getAccountModels(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountModelsResponse>({
    url: '/api/admin/accounts/models',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getAccountModelCatalog(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountModelCatalogResponse>({
    url: '/api/admin/accounts/models/catalog',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function refreshAccountModels(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountModelsResponse>({
    url: '/api/admin/accounts/models/refresh',
    method: 'POST',
    data,
    ...options,
  })
}
