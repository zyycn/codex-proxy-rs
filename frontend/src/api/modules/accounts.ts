import { z } from 'zod'

import request, { requestParsed } from '../request'

const ACCOUNT_EXPORT_CONFIRMATION = 'export_sensitive_accounts'

const quotaWindowLocalUsageSchema = z.object({
  requestCount: z.number(),
  requestCountDisplay: z.string(),
  inputTokens: z.number(),
  inputTokensDisplay: z.string(),
  outputTokens: z.number(),
  outputTokensDisplay: z.string(),
  cachedTokens: z.number(),
  cachedTokensDisplay: z.string(),
  totalTokens: z.number(),
  totalTokensDisplay: z.string(),
})

const quotaWindowSchema = z.object({
  key: z.string(),
  group: z.enum(['monthly', 'shortTerm', 'other']),
  windowSeconds: z.number().nullable(),
  labelDisplay: z.string(),
  usedPercent: z.number().nullable(),
  usedPercentDisplay: z.string(),
  localUsage: quotaWindowLocalUsageSchema.optional(),
  resetAtDisplay: z.string(),
  windowUsedDisplay: z.string(),
})

const accountQuotaSchema = z.object({
  refreshedAtDisplay: z.string(),
  windows: z.array(quotaWindowSchema),
})

const accountModelUsageSchema = z
  .object({
    model: z.string(),
    requestCount: z.number(),
    requestCountDisplay: z.string(),
    successRate: z.number(),
    successRateDisplay: z.string(),
    inputTokens: z.number(),
    inputTokensDisplay: z.string(),
    outputTokens: z.number(),
    outputTokensDisplay: z.string(),
    cachedTokens: z.number(),
    cachedTokensDisplay: z.string(),
    totalTokens: z.number(),
    totalTokensDisplay: z.string(),
    billingAmountUsd: z.number(),
    billingAmountUsdDisplay: z.string(),
    lastUsedAt: z.string().nullable(),
    lastUsedAtDisplay: z.string(),
  })
  .passthrough()

const accountUsageSchema = z
  .object({
    inputTokensDisplay: z.string(),
    outputTokensDisplay: z.string(),
    cachedTokensDisplay: z.string(),
    createdTokensDisplay: z.string(),
    readTokensDisplay: z.string(),
    models: z.array(accountModelUsageSchema),
  })
  .passthrough()

const accountSchema = z
  .object({
    id: z.string(),
    email: z.string().nullable(),
    accountId: z.string().nullable(),
    userId: z.string().nullable(),
    label: z.string().nullable(),
    planType: z.string().nullable(),
    hasRefreshToken: z.boolean(),
    status: z.string(),
    displayStatus: z.string(),
    tokenRefreshing: z.boolean(),
    accessTokenExpiresAt: z.string().nullable(),
    accessTokenExpiresAtDisplay: z.string().nullable(),
    addedAt: z.string(),
    addedAtDisplay: z.string(),
    updatedAt: z.string(),
    updatedAtDisplay: z.string(),
    quota: accountQuotaSchema,
    usage: accountUsageSchema,
  })
  .passthrough()

const accountPageSchema = z.object({
  items: z.array(accountSchema),
  page: z.object({
    page: z.number(),
    pageSize: z.number(),
    total: z.number(),
    totalPages: z.number(),
  }),
  summary: z.object({
    total: z.number(),
    active: z.number(),
    quotaExhausted: z.number(),
    attention: z.number(),
  }),
})

const _accountQuerySchema = z.object({
  page: z.number().optional(),
  pageSize: z.number().optional(),
  search: z.string().optional(),
  status: z.string().optional(),
  sortBy: z.string().optional(),
  sortDirection: z.enum(['asc', 'desc']).optional(),
})

const importResultSchema = z.object({
  imported: z.number(),
  skipped: z.number(),
  sourceFormat: z.string(),
})

const oauthAuthorizeSchema = z.object({
  sessionId: z.string(),
  authUrl: z.string(),
  expiresAt: z.string(),
  expiresAtDisplay: z.string(),
})

const accountRefreshSchema = z.object({
  id: z.string(),
  email: z.string().nullable(),
  previousStatus: z.string(),
  result: z.enum(['alive', 'dead', 'skipped']),
  error: z.string().nullable(),
  durationMs: z.number(),
})

const accountQuotaRefreshSchema = z
  .object({
    quota: z.unknown(),
    raw: z.unknown(),
    quotaData: accountQuotaSchema,
    planType: z.string().nullable(),
    account: accountSchema,
  })
  .passthrough()

const accountModelsSchema = z.object({
  models: z.array(
    z.object({
      id: z.string(),
      label: z.string(),
    }),
  ),
})

type JsonObject = Record<string, unknown>

export function getAccounts(params?: z.input<typeof _accountQuerySchema>) {
  return requestParsed({
    url: '/api/admin/accounts',
    method: 'GET',
    params,
  }, accountPageSchema)
}

export function importAccounts(data: JsonObject) {
  return requestParsed({
    url: '/api/admin/accounts/import',
    method: 'POST',
    data,
  }, importResultSchema)
}

export function exportAccounts(params?: { ids?: string }) {
  return request({
    url: '/api/admin/accounts/export',
    method: 'GET',
    params: {
      ...params,
      confirm: ACCOUNT_EXPORT_CONFIRMATION,
    },
  })
}

export function authorizeAccountOAuth(data = {}) {
  return requestParsed({
    url: '/api/admin/accounts/oauth/authorize',
    method: 'POST',
    data,
  }, oauthAuthorizeSchema)
}

export function exchangeAccountOAuth(data: JsonObject) {
  return requestParsed({
    url: '/api/admin/accounts/oauth/exchange',
    method: 'POST',
    data,
  }, importResultSchema)
}

export function deleteAccounts(data: { ids: string[] }) {
  return request({
    url: '/api/admin/accounts/delete',
    method: 'POST',
    data,
  })
}

export function refreshAccount(data: { id: string }) {
  return requestParsed({
    url: '/api/admin/accounts/refresh',
    method: 'POST',
    data,
  }, accountRefreshSchema)
}

export function updateAccount(data: { id: string, status?: string, label?: string | null }) {
  return request({
    url: '/api/admin/accounts/update',
    method: 'POST',
    data,
  })
}

export function getAccountQuota(params: { id: string }) {
  return requestParsed({
    url: '/api/admin/accounts/quota',
    method: 'GET',
    params,
  }, accountQuotaRefreshSchema)
}

export function getAccountModels(data: { id: string }) {
  return requestParsed({
    url: '/api/admin/accounts/models',
    method: 'GET',
    params: {
      id: data.id,
    },
  }, accountModelsSchema)
}

export function refreshAccountModels(data: { id: string }) {
  return requestParsed({
    url: '/api/admin/accounts/models',
    method: 'POST',
    data,
  }, accountModelsSchema)
}
