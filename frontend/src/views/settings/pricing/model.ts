import type { ModelPricing, PriceBand, PricingCatalog, TokenPrices } from '@/api'
import { computed } from 'vue'
import { t } from '@/i18n'

export const bands = computed<{ value: PriceBand, label: string }[]>(() => ([
  {
    value: 'standard',
    label: t('settings.pricing.standard'),
  },
  { value: 'fast', label: 'Fast' },
  { value: 'ultrafast', label: 'Ultrafast' },
  { value: 'flex', label: 'Flex' },
  {
    value: 'long_standard',
    label: t('settings.pricing.longContextStandard'),
  },
  {
    value: 'long_fast',
    label: t('settings.pricing.longContextPriority'),
  },
  {
    value: 'long_ultrafast',
    label: t('settings.pricing.longContextUltrafast'),
  },
  {
    value: 'long_flex',
    label: t('settings.pricing.longContextFlex'),
  },
  {
    value: 'image',
    label: t('settings.pricing.imageToken'),
  },
]))
export const priceFields = computed<{ key: keyof TokenPrices, label: string }[]>(() => ([
  {
    key: 'input',
    label: t('settings.pricing.input'),
  },
  {
    key: 'output',
    label: t('settings.pricing.output'),
  },
  {
    key: 'cacheRead',
    label: t('settings.pricing.cacheRead'),
  },
  {
    key: 'cacheWrite',
    label: t('settings.pricing.cacheWrites'),
  },
]))
export interface PricingRow {
  model: string
  provider: string
  source: 'builtin' | 'synced' | 'custom'
  canDelete: boolean
  base: ModelPricing
  custom?: ModelPricing
  effective: ModelPricing
}
export function pricingProviders(catalog: PricingCatalog): string[] {
  return Object.keys(catalog.defaults).sort((left, right) => left.localeCompare(right))
}
export function pricingRows(catalog: PricingCatalog, provider: string): PricingRow[] {
  const defaults = catalog.defaults[provider] ?? {}
  const synced = catalog.synced[provider] ?? {}
  const custom = catalog.overrides[provider] ?? {}
  return [
    ...new Set([
      ...Object.keys(defaults),
      ...Object.keys(synced),
      ...Object.keys(custom),
    ]),
  ].sort().map((model) => {
    const base: ModelPricing = {
      multiplierBps: 10_000,
      bands: {
        ...defaults[model]?.bands,
        ...synced[model]?.bands,
      },
    }
    const override = custom[model]
    return {
      model,
      provider,
      base,
      custom: override,
      source: override
        ? 'custom'
        : synced[model] ? 'synced' : 'builtin',
      canDelete: !Object.hasOwn(defaults, model),
      effective: {
        multiplierBps: override?.multiplierBps ?? 10_000,
        bands: {
          ...base.bands,
          ...override?.bands,
        },
      },
    }
  })
}
export const sourceLabels = computed(() => ({
  builtin: t('settings.pricing.builtInPriceList'),
  synced: t('settings.pricing.syncPriceList'),
  custom: t('settings.pricing.manualCoverage'),
}))
export function parseMultiplier(value: string): number | undefined {
  if (!/^\d{1,3}(?:\.\d{1,4})?$/.test(value))
    return undefined
  const [whole = '0', fraction = ''] = value.split('.')
  const bps = Number(whole) * 10_000 + Number(fraction.padEnd(4, '0'))
  return bps <= 1_000_000
    ? bps
    : undefined
}
export function validPrice(value: string) {
  return /^\d{1,7}(?:\.\d{1,4})?$/.test(value) && Number(value) <= 1_000_000
}
export function multiplierText(bps: number) {
  return `${bps / 10_000}×`
}
export function effectivePrice(value: string | undefined, bps = 10_000): string {
  if (value === undefined || !validPrice(value))
    return t('settings.pricing.notConfigured')
  const [whole = '0', fraction = ''] = value.split('.')
  const scaled = (BigInt(whole) * 10_000n + BigInt(fraction.padEnd(4, '0'))) * BigInt(bps)
  const decimal = (scaled % 100_000_000n).toString().padStart(8, '0').replace(/0+$/, '')
  return `${scaled / 100_000_000n}${decimal
    ? `.${decimal}`
    : ''}`
}
