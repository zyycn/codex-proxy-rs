import type { Component } from 'vue'
import type { BuiltinProvider } from './catalog'
import type { AccountConnectionUpdate } from '@/api'
import { t } from '@/i18n'
import { isBuiltinProvider } from './catalog'
import { splitDocument, tokenDocuments } from './openai/accounts/import'
import OpenAiAccountConnectionEditor from './openai/accounts/OpenAiAccountConnectionEditor.vue'
import OpenAiApiKeyImportForm from './openai/accounts/OpenAiApiKeyImportForm.vue'

export type AccountImportMode = 'oauth' | 'api_key' | 'access_token' | 'refresh_token' | 'json'
type Document = Record<string, unknown>

export interface AccountConnectionForm {
  readonly formModel: object
  readonly canSave: boolean
  readonly change?: AccountConnectionUpdate
}

interface ProviderAccounts {
  modes: readonly AccountImportMode[]
  imageModelPresets?: readonly string[]
  callback: () => { callbackLabel: string, callbackPlaceholder: string, description: string }
  apiKeyImportForm?: Component
  connectionEditor?: Component & (new () => AccountConnectionForm)
  tokenDocuments?: (mode: 'access_token' | 'refresh_token', text: string) => Document[]
  splitDocument?: (document: Document) => Document[]
}

const providers: Record<BuiltinProvider, ProviderAccounts> = {
  openai: {
    modes: [
      'oauth',
      'api_key',
      'access_token',
      'refresh_token',
      'json',
    ],
    callback: () => ({
      callbackLabel: t('accounts.onboarding.callbackAddress'),
      callbackPlaceholder: 'http://localhost:1455/auth/callback?code=...&state=...',
      description: t('accounts.onboarding.callbackHelp'),
    }),
    apiKeyImportForm: OpenAiApiKeyImportForm,
    connectionEditor: OpenAiAccountConnectionEditor,
    // Codex 原生生图使用 gpt-image-2，另保留 gpt-image-1.5 供客户端显式选择
    imageModelPresets: ['gpt-image-2', 'gpt-image-1.5'],
    tokenDocuments,
    splitDocument,
  },
  xai: {
    modes: ['oauth', 'json'],
    callback: () => ({
      callbackLabel: t('accounts.onboarding.callbackLabel'),
      callbackPlaceholder: t('accounts.onboarding.callbackPlaceholder'),
      description: t('accounts.onboarding.callbackHelp'),
    }),
  },
  antigravity: {
    modes: ['oauth', 'json'],
    callback: fullCallback,
  },
}

export function providerAccounts(provider?: string) {
  return isBuiltinProvider(provider)
    ? providers[provider]
    : undefined
}

function fullCallback() {
  return {
    callbackLabel: t('accounts.onboarding.callbackAddress'),
    callbackPlaceholder: t('accounts.onboarding.fullCallbackPlaceholder'),
    description: t('accounts.onboarding.fullCallbackHelp'),
  }
}

export function oauthCallback(provider?: string) {
  return (providerAccounts(provider)?.callback ?? fullCallback)()
}

export function importModeOptions(provider?: string) {
  const labels: Record<AccountImportMode, string> = {
    oauth: 'OAuth',
    api_key: t('common.fields.apiKey'),
    access_token: 'AT',
    refresh_token: 'RT',
    json: t('accounts.onboarding.accountFile'),
  }
  return (providerAccounts(provider)?.modes ?? []).map(value => ({ value, label: labels[value] }))
}
