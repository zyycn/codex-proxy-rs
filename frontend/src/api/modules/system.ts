import request from '../request'

export interface SystemVersion {
  version: string
  gitSha: string
  buildTime: string
  deploymentMode: string
  deploymentModeLabel: string
  updateChannel: string
  latestVersion: string
  hasUpdate: boolean
  updateCached: boolean
  updateWarning: string | null
}

export interface SystemUpdateDetail {
  currentVersion: string
  latestVersion: string
  hasUpdate: boolean
  deploymentMode: string
  deploymentModeLabel: string
  buildType: string
  buildTypeLabel: string
  releaseUrl: string | null
  notes: string | null
  cached: boolean
  updateSupported: boolean
  unsupportedReason: string | null
  warning: string | null
}

export interface SystemUpdateAccepted {
  operationId: string
  deploymentMode: string
  message: string
  needRestart: boolean
  targetVersion: string
}

export interface SystemRestartAccepted {
  message: string
  operationId: string
}

export function getSystemVersion(timeout = 0) {
  return request<SystemVersion>({
    url: '/api/admin/system/version',
    method: 'GET',
    ...(timeout ? { timeout } : {}),
  })
}

interface SystemUpdateDetailQuery {
  refresh?: boolean
}

interface SystemUpdateTarget {
  targetVersion?: string
}

export function getSystemUpdateDetail(data: SystemUpdateDetailQuery) {
  return request<SystemUpdateDetail>({
    url: '/api/admin/system/update/detail',
    method: 'GET',
    params: data,
  })
}

export function performSystemUpdate(data: SystemUpdateTarget) {
  return request<SystemUpdateAccepted>({
    url: '/api/admin/system/update',
    method: 'POST',
    data,
  })
}

export function restartSystem() {
  return request<SystemRestartAccepted>({
    url: '/api/admin/system/restart',
    method: 'POST',
  })
}
