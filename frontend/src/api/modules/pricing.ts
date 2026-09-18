import type { RequestOptions } from '../request'
import request from '../request'

export type PriceBand = 'standard' | 'fast' | 'flex' | 'long_standard' | 'long_fast' | 'long_flex' | 'image'

export interface TokenPrices {
  input: string
  output: string
  cacheRead: string
  cacheWrite: string
}

export interface ModelPricing {
  multiplierBps: number
  bands: Partial<Record<PriceBand, TokenPrices>>
}

export type PricingMap = Record<string, Record<string, ModelPricing>>

export interface PricingCatalog {
  defaults: PricingMap
  synced: PricingMap
  overrides: PricingMap
  syncedAt: string | null
}

export type PricingChange
  = | { action: 'replace', pricing: ModelPricing }
    | { action: 'multiplier', multiplierBps: number }
    | { action: 'reset' }

export interface PricingSyncPreview {
  prices: PricingMap
  skipped: string[]
}

interface UpdatePricingParam {
  provider: string
  models: string[]
  change: PricingChange
}

interface PricingMutationResponse {
  saved: boolean
}

export function getPricing(options: RequestOptions = {}) {
  return request<PricingCatalog>({
    url: '/api/admin/settings/pricing',
    method: 'GET',
    ...options,
  })
}

export function updatePricing(data: UpdatePricingParam) {
  return request<PricingMutationResponse>({
    url: '/api/admin/settings/pricing/update',
    method: 'POST',
    data,
  })
}

export function previewPricingSync() {
  return request<PricingSyncPreview>({
    url: '/api/admin/settings/pricing/sync/preview',
    method: 'POST',
    timeout: 40_000,
  })
}

export function syncPricing(data: PricingSyncPreview) {
  return request<PricingMutationResponse>({
    url: '/api/admin/settings/pricing/sync',
    method: 'POST',
    data,
    timeout: 45_000,
  })
}
