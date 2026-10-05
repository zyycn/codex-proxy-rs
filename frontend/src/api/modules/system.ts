import type { RequestOptions } from '../request'
import request from '../request'

export type SystemUpdateChannel = 'stable' | 'rc' | 'beta' | 'alpha' | 'exp'

export interface SystemUpdatePolicy {
  channel: SystemUpdateChannel
  availableChannels: SystemUpdateChannel[]
}

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
  policy: SystemUpdatePolicy
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
  restartConfirmationSupported?: boolean
  unsupportedReason: string | null
  warning: string | null
}

export interface SystemUpdateAccepted {
  operationId: string
  deploymentMode: string
  message: string
  targetVersion: string
}

export interface SystemUpdateStatus {
  previousVersion: string | null
  currentVersion: string | null
  needRestart: boolean
  operation: {
    operationId: string | null
    kind: 'update' | 'rollback' | 'restart' | null
    status: 'idle' | 'running' | 'succeeded' | 'failed'
    targetVersion: string | null
    message: string | null
    error: string | null
    startedAt: string | null
    startedAtDisplay: string | null
    finishedAt: string | null
    finishedAtDisplay: string | null
  }
}

export interface SystemRestartAccepted {
  message: string
  operationId: string
}

export function getSystemVersion(options: RequestOptions = {}) {
  return request<SystemVersion>({
    url: '/api/admin/system/version',
    method: 'GET',
    ...options,
  })
}

interface SystemUpdateDetailQuery {
  refresh?: boolean
  channel?: SystemUpdateChannel
}

interface SystemUpdateTarget {
  targetVersion: string
  channel: SystemUpdateChannel
}

export function getSystemUpdateDetail(data: SystemUpdateDetailQuery) {
  return request<SystemUpdateDetail>({
    url: '/api/admin/system/update/detail',
    method: 'GET',
    params: data,
  })
}

export function performSystemUpdate(data: SystemUpdateTarget, options: RequestOptions = {}) {
  return request<SystemUpdateAccepted>({
    url: '/api/admin/system/update',
    method: 'POST',
    data,
    ...options,
  })
}

export interface SystemRestartPlan {
  targetVersion: string | null
  releaseManifestSha256: string | null
  configRevision: number
  incompatiblePlugins: { instanceId: string, name: string, reason: string }[]
}

export function checkSystemRestart() {
  return request<SystemRestartPlan>({ url: '/api/admin/system/restart/check', method: 'GET' })
}

export function restartSystem(confirmation?: SystemRestartPlan, options: RequestOptions = {}) {
  return request<SystemRestartAccepted>({
    url: '/api/admin/system/restart',
    method: 'POST',
    data: { confirmation },
    ...options,
  })
}

export function getSystemUpdateStatus(options: RequestOptions = {}) {
  return request<SystemUpdateStatus>({
    url: '/api/admin/system/update/status',
    method: 'GET',
    ...options,
  })
}
