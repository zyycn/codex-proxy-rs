import { z } from 'zod'

import { requestParsed } from '../request'

const versionSchema = z.object({
  version: z.string(),
  gitSha: z.string(),
  buildTime: z.string(),
  deploymentMode: z.string(),
  deploymentModeLabel: z.string(),
  updateChannel: z.string(),
  latestVersion: z.string(),
  hasUpdate: z.boolean(),
  updateCached: z.boolean(),
  updateWarning: z.string().nullable(),
})

const updateDetailSchema = z.object({
  currentVersion: z.string(),
  latestVersion: z.string(),
  hasUpdate: z.boolean(),
  deploymentMode: z.string(),
  deploymentModeLabel: z.string(),
  buildType: z.string(),
  buildTypeLabel: z.string(),
  releaseUrl: z.string().nullable(),
  notes: z.string().nullable(),
  cached: z.boolean(),
  updateSupported: z.boolean(),
  unsupportedReason: z.string().nullable(),
  warning: z.string().nullable(),
})

const updateStartedSchema = z.object({
  operationId: z.string(),
  deploymentMode: z.string(),
  message: z.string(),
  needRestart: z.boolean(),
  targetVersion: z.string(),
})

const restartResultSchema = z.object({
  message: z.string(),
  operationId: z.string(),
})

export function getSystemVersion(options: { timeoutMs?: number } = {}) {
  return requestParsed({
    url: '/api/admin/system/version',
    method: 'GET',
    ...(options.timeoutMs ? { timeout: options.timeoutMs } : {}),
  }, versionSchema)
}

export function getSystemUpdateDetail(refresh = false) {
  return requestParsed({
    url: '/api/admin/system/update-detail',
    method: 'GET',
    params: { refresh },
  }, updateDetailSchema)
}

export function performSystemUpdate(targetVersion: string) {
  return requestParsed({
    url: '/api/admin/system/update',
    method: 'POST',
    data: { targetVersion },
  }, updateStartedSchema)
}

export function restartSystem() {
  return requestParsed({
    url: '/api/admin/system/restart',
    method: 'POST',
  }, restartResultSchema)
}
