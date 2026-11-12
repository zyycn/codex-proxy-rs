import type { ApiKeyAccountForm } from './upstreamApiKey'
import type { AccountModelAccess } from '@/api'
import { t } from '@/i18n'
import { accountSchedulingValues } from './schedulingForm'
import { emptyApiKeyAccountForm } from './upstreamApiKey'

export type AccountCreateSource = { kind: 'bundle' } | { kind: 'provider', id: string }
export type AccountImportMode = 'oauth' | 'api_key' | 'access_token' | 'refresh_token' | 'json'
type AccountImportInputMode = 'access_token' | 'refresh_token' | 'json'

export interface AccountCreateForm {
  name: string
  source: AccountCreateSource | null
  apiKey: ApiKeyAccountForm
  notes: string
  enabled: boolean
  concurrencyLimit: string
  weight: string
  modelAccess?: AccountModelAccess
  groupIds: string[]
  step: 'settings' | 'import'
  mode: AccountImportMode
  importTexts: Record<AccountImportInputMode, string>
  proxyMode: string
  proxyId: string
}

export function emptyAccountCreateForm(): AccountCreateForm {
  return {
    name: '',
    source: null,
    apiKey: emptyApiKeyAccountForm(),
    notes: '',
    enabled: true,
    concurrencyLimit: '',
    weight: '1',
    groupIds: [],
    step: 'settings',
    mode: 'oauth',
    importTexts: { access_token: '', refresh_token: '', json: '' },
    proxyMode: 'deployment',
    proxyId: '',
  }
}

export function accountCreateProvider(form: AccountCreateForm) {
  return form.source?.kind === 'provider'
    ? form.source.id
    : undefined
}

export function accountCreateSourceKey(form: AccountCreateForm) {
  return form.source?.kind === 'provider'
    ? `provider:${form.source.id}`
    : form.source?.kind ?? ''
}

export function accountImportSettings(form: AccountCreateForm) {
  const scheduling = accountSchedulingValues(form.concurrencyLimit, form.weight)
  return {
    name: form.source?.kind === 'provider'
      ? form.name.trim() || undefined
      : undefined,
    modelAccess: form.modelAccess
      ? {
          ...form.modelAccess,
          models: [...form.modelAccess.models],
        }
      : undefined,
    enabled: form.enabled,
    ...scheduling,
    groupIds: [
      ...new Set(form.groupIds),
    ],
    notes: form.notes.trim() || undefined,
  }
}

export function accountImportModes(provider: string | undefined) {
  if (provider === 'openai') {
    return [
      { label: 'OAuth', value: 'oauth' as const },
      {
        label: t('common.fields.apiKey'),
        value: 'api_key' as const,
      },
      { label: 'AT', value: 'access_token' as const },
      { label: 'RT', value: 'refresh_token' as const },
      {
        label: t('accounts.onboarding.accountFile'),
        value: 'json' as const,
      },
    ]
  }
  if (provider === 'xai') {
    return [
      { label: 'OAuth', value: 'oauth' as const },
      {
        label: t('accounts.onboarding.accountFile'),
        value: 'json' as const,
      },
    ]
  }
  return []
}
