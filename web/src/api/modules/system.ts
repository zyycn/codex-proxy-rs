import request, { type ApiPayload } from '../request'

export interface SystemVersion {
  version: string
  gitSha: string
  buildTime: string
  deploymentMode: string
  image?: string
  updateChannel: string
}

export interface SystemUpdateInfo {
  currentVersion: string
  latestVersion: string
  hasUpdate: boolean
  deploymentMode: string
  releaseUrl?: string
  notes?: string
  cached: boolean
  updateSupported: boolean
  unsupportedReason?: string
  targetImage?: string
  requiresBackup: boolean
  warning?: string
}

export interface SystemUpdateStarted {
  operationId: string
  deploymentMode: string
  message: string
  needReconnect: boolean
  targetVersion: string
  targetImage?: string
}

export function getSystemVersion() {
  return request<SystemVersion>({
    url: '/api/admin/system/version',
    method: 'GET',
  })
}

export function checkSystemUpdates(force = false) {
  return request<SystemUpdateInfo>({
    url: '/api/admin/system/check-updates',
    method: 'GET',
    params: { force },
  })
}

export function performSystemUpdate(data: ApiPayload = {}) {
  return request<SystemUpdateStarted>({
    url: '/api/admin/system/update',
    method: 'POST',
    data,
  })
}

export function rollbackSystemUpdate() {
  return request({
    url: '/api/admin/system/rollback',
    method: 'POST',
  })
}

export function restartSystem() {
  return request({
    url: '/api/admin/system/restart',
    method: 'POST',
  })
}
