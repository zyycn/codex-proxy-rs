export const PROVIDER_DISPLAY_NAMES = {
  openai: 'OpenAI',
  xai: 'xAI',
} as const

export type SupportedProvider = keyof typeof PROVIDER_DISPLAY_NAMES

export function isSupportedProvider(value: unknown): value is SupportedProvider {
  return typeof value === 'string' && value in PROVIDER_DISPLAY_NAMES
}

export function providerDisplayName(value?: string | null) {
  return isSupportedProvider(value) ? PROVIDER_DISPLAY_NAMES[value] : undefined
}

export function formatProviderLabel(value?: string | null, fallback = '—') {
  const normalized = value?.trim()
  return providerDisplayName(normalized?.toLowerCase()) ?? (normalized || fallback)
}
