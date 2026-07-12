import request from '../request'
import { API_BASE_URL } from '../constants'

export const SYSTEM_UPDATE_EVENTS_URL = `${API_BASE_URL}/api/admin/system/update-events`
const SYSTEM_HEALTH_URL = `${API_BASE_URL}/healthz`

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
  updateWarning?: string
}

export interface SystemUpdateInfo {
  currentVersion: string
  latestVersion: string
  hasUpdate: boolean
  deploymentMode: string
  deploymentModeLabel: string
  buildType: string
  buildTypeLabel: string
  releaseUrl?: string
  notes?: string
  cached: boolean
  updateSupported: boolean
  unsupportedReason?: string
  warning?: string
}

export interface SystemUpdateStarted {
  operationId: string
  deploymentMode: string
  message: string
  needRestart: boolean
  targetVersion: string
}

export interface SystemOperationStarted {
  operationId?: string
  message: string
  needRestart?: boolean
}

export function getSystemVersion() {
  return request<SystemVersion>({
    url: '/api/admin/system/version',
    method: 'GET',
  })
}

export async function checkSystemHealth() {
  const response = await fetch(SYSTEM_HEALTH_URL, { cache: 'no-store' })
  if (!response.ok) {
    throw new Error('service is not ready')
  }
}

export function getSystemUpdateDetail(refresh = false) {
  return request<SystemUpdateInfo>({
    url: '/api/admin/system/update-detail',
    method: 'GET',
    params: { refresh },
  })
}

export function performSystemUpdate(targetVersion: string) {
  return request<SystemUpdateStarted>({
    url: '/api/admin/system/update',
    method: 'POST',
    data: { targetVersion },
  })
}

export function restartSystem() {
  return request<SystemOperationStarted>({
    url: '/api/admin/system/restart',
    method: 'POST',
  })
}
