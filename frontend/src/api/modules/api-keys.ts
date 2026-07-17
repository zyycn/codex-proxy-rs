import { z } from 'zod'

import { requestParsed } from '../request'

const apiKeySchema = z.object({
  id: z.string(),
  name: z.string(),
  label: z.string().nullable(),
  prefix: z.string(),
  key: z.string(),
  enabled: z.boolean(),
  createdAt: z.string(),
  createdAtDisplay: z.string(),
  lastUsedAt: z.string().nullable(),
  lastUsedAtDisplay: z.string(),
})

const pageSchema = z.object({
  page: z.number(),
  pageSize: z.number(),
  total: z.number(),
  totalPages: z.number(),
})

const apiKeyPageSchema = z.object({
  items: z.array(apiKeySchema),
  page: pageSchema,
})

const _apiKeyQuerySchema = z.object({
  page: z.number().optional(),
  pageSize: z.number().optional(),
  search: z.string().optional(),
  sortBy: z.string().optional(),
  sortDirection: z.enum(['asc', 'desc']).optional(),
})

const _createApiKeySchema = z.object({
  name: z.string(),
  label: z.string().optional(),
})
const _deleteApiKeysSchema = z.object({ ids: z.array(z.string()) })
const _updateApiKeySchema = z
  .object({
    id: z.string(),
    label: z.string().nullable().optional(),
    status: z.enum(['active', 'disabled']).optional(),
  })
  .refine(value => value.label !== undefined || value.status !== undefined)

const batchDeleteResultSchema = z.object({
  deleted: z.number(),
})

export function getApiKeys(params: z.input<typeof _apiKeyQuerySchema>) {
  return requestParsed({
    url: '/api/admin/keys',
    method: 'GET',
    params,
  }, apiKeyPageSchema)
}

export function createApiKey(data: z.input<typeof _createApiKeySchema>) {
  return requestParsed({
    url: '/api/admin/keys',
    method: 'POST',
    data,
  }, apiKeySchema)
}

export function deleteApiKeys(data: z.input<typeof _deleteApiKeysSchema>) {
  return requestParsed({
    url: '/api/admin/keys/delete',
    method: 'POST',
    data,
  }, batchDeleteResultSchema)
}

export function updateApiKey(data: z.input<typeof _updateApiKeySchema>) {
  return requestParsed({
    url: '/api/admin/keys/update',
    method: 'POST',
    data,
  }, apiKeySchema)
}
