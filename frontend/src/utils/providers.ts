import type { Component } from 'vue'
import { Key, LinkAlt, Openai, Xai } from '@boxicons/vue'
import { Fingerprint, Plug } from '@lucide/vue'

export const PROVIDER_DISPLAY_NAMES = {
  openai: 'OpenAI',
  xai: 'xAI',
} as const

export type SupportedProvider = keyof typeof PROVIDER_DISPLAY_NAMES

export function isSupportedProvider(value: unknown): value is SupportedProvider {
  return typeof value === 'string' && Object.hasOwn(PROVIDER_DISPLAY_NAMES, value)
}

export function providerDisplayName(value?: string | null) {
  return isSupportedProvider(value) ? PROVIDER_DISPLAY_NAMES[value] : undefined
}

export function formatProviderLabel(value?: string | null, fallback = '—') {
  const normalized = value?.trim()
  return providerDisplayName(normalized?.toLowerCase()) ?? (normalized || fallback)
}

export function providerIcon(value: string): Component {
  switch (value.trim().toLowerCase()) {
    case 'openai': return Openai
    case 'xai': return Xai
    default: return Plug
  }
}

export function formatAuthenticationLabel(value?: string | null) {
  const normalized = value?.trim()
  switch (normalized?.toLowerCase()) {
    case 'oauth': return 'OAuth'
    case 'api_key': return 'API Key'
    default: return normalized ? `自定义认证 · ${normalized}` : '未知认证类型'
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
