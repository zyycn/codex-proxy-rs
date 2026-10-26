import type { Account, ApiKeyConfiguration } from '@/api'
import { t } from '@/i18n'

export function isOpenAiApiKeyAccount(account: Pick<Account, 'provider' | 'authenticationKind'> | null | undefined): boolean {
  return account?.provider === 'openai' && account.authenticationKind === 'api_key'
}

export function isOpenAiOAuthAccount(account: Pick<Account, 'provider' | 'authenticationKind'> | null | undefined): boolean {
  return account?.provider === 'openai' && account.authenticationKind === 'oauth'
}

export interface ApiKeyAccountForm extends Omit<ApiKeyConfiguration, 'transport'> {
  name: string
  apiKey: string
  transport: ApiKeyConfiguration['transport'] | ''
}

export function emptyApiKeyAccountForm(): ApiKeyAccountForm {
  return {
    name: '',
    base_url: '',
    apiKey: '',
    transport: 'http',
  }
}

export function parseApiKeyConfiguration(value: Record<string, unknown> | undefined): ApiKeyConfiguration | undefined {
  if (!value || typeof value.base_url !== 'string' || (value.transport !== 'http' && value.transport !== 'prefer_websocket'))
    return undefined
  return { base_url: value.base_url, transport: value.transport }
}

export function apiKeyAccountError(form: ApiKeyAccountForm, editing = false): string | undefined {
  if (!editing && !form.name.trim())
    return t('accounts.credentials.nameRequired')
  if (!form.transport)
    return t('accounts.credentials.validation.transportRequired')
  if (form.base_url.trim().length > 2048)
    return t('accounts.credentials.validation.urlLength')
  try {
    const url = new URL(form.base_url)
    if (!['https:', 'http:'].includes(url.protocol) || !url.hostname || url.username || url.password || url.search || url.hash)
      return t('accounts.credentials.validation.urlFormat')
  }
  catch {
    return t('accounts.credentials.validation.urlRequired')
  }
  if (!editing && !form.apiKey)
    return t('accounts.credentials.apiKeyRequired')
  if (form.apiKey && (!/^[\x21-\x7E]+$/.test(form.apiKey) || form.apiKey.length > 16384))
    return t('accounts.credentials.validation.apiKeyFormat')
  return undefined
}
