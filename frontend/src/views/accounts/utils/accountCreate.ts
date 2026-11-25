import type { AccountModelAccess } from '@/api'
import type { AccountImportMode } from '@/providers/accounts'
import { accountSchedulingValues } from './schedulingForm'

export type AccountCreateSource = { kind: 'bundle' } | { kind: 'provider', id: string }
type AccountImportInputMode = 'access_token' | 'refresh_token' | 'json'

export interface AccountCreateForm {
  name: string
  source: AccountCreateSource | null
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
