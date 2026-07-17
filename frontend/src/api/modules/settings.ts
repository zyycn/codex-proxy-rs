import { z } from 'zod'

import { requestParsed } from '../request'

const rotationStrategySchema = z.enum([
  'smart',
  'quota_reset_priority',
  'round_robin',
  'sticky',
])

const settingsSchema = z.object({
  modelAliases: z.record(z.string(), z.string()),
  refreshMarginSeconds: z.number(),
  refreshConcurrency: z.number(),
  maxConcurrentPerAccount: z.number(),
  requestIntervalMs: z.number(),
  rotationStrategy: rotationStrategySchema,
})

const adminApiKeyStatusSchema = z.object({
  exists: z.boolean(),
})

const generatedAdminApiKeySchema = z.object({
  key: z.string(),
})

const messageSchema = z.object({
  message: z.string(),
})

export function getSettings() {
  return requestParsed({
    url: '/api/admin/settings',
    method: 'GET',
  }, settingsSchema)
}

export function updateSettings(data: z.input<typeof settingsSchema>) {
  return requestParsed({
    url: '/api/admin/settings',
    method: 'POST',
    data,
  }, settingsSchema)
}

export function getAdminApiKeyStatus() {
  return requestParsed({
    url: '/api/admin/settings/admin-api-key',
    method: 'GET',
  }, adminApiKeyStatusSchema)
}

export function regenerateAdminApiKey() {
  return requestParsed({
    url: '/api/admin/settings/admin-api-key/regenerate',
    method: 'POST',
  }, generatedAdminApiKeySchema)
}

export function deleteAdminApiKey() {
  return requestParsed({
    url: '/api/admin/settings/admin-api-key',
    method: 'DELETE',
  }, messageSchema)
}
