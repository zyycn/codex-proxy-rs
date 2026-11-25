import type { Component } from 'vue'
import { GoogleAntigravity, Key, LinkAlt, Openai, Xai } from '@boxicons/vue'
import { Fingerprint, Plug } from '@lucide/vue'
import { t } from '@/i18n'

const providers = {
  openai: { label: 'OpenAI', icon: Openai },
  xai: { label: 'xAI', icon: Xai },
  antigravity: { label: 'Antigravity', icon: GoogleAntigravity },
} as const

export const PROVIDER_IDS = Object.keys(providers) as BuiltinProvider[]

export type BuiltinProvider = keyof typeof providers

export function isBuiltinProvider(value: unknown): value is BuiltinProvider {
  return typeof value === 'string' && Object.hasOwn(providers, value)
}

export function formatProviderLabel(value?: string | null, fallback = '—') {
  const normalized = value?.trim()
  const provider = normalized?.toLowerCase()
  return isBuiltinProvider(provider)
    ? providers[provider].label
    : (normalized || fallback)
}

export function providerIcon(value: string): Component {
  const id = value.trim().toLowerCase()
  return isBuiltinProvider(id)
    ? providers[id].icon
    : Plug
}

export function formatAuthenticationLabel(value?: string | null) {
  const normalized = value?.trim()
  switch (normalized?.toLowerCase()) {
    case 'oauth': return 'OAuth'
    case 'api_key': return t('common.fields.apiKey')
    default: return normalized
      ? t('common.providers.customAuth', { normalized })
      : t('common.providers.unknownAuth')
  }
}

export function authenticationIcon(value?: string | null): Component | undefined {
  switch (value?.trim().toLowerCase()) {
    case 'oauth': return LinkAlt
    case 'api_key': return Key
    case undefined:
    case '': return undefined
    default: return Fingerprint
  }
}
