import type { AccountModelAccess } from '@/api'
import { t } from '@/i18n'

export function accountModelAccessError(value: AccountModelAccess | undefined): string | undefined {
  if (!value || value.mode === 'all')
    return undefined
  if (value.models.length === 0)
    return t('accounts.modelAccess.selectOrAddAtLeastOneModel')
  if (value.models.length > 256)
    return t('accounts.modelAccess.configureUpToModels')
  if (value.models.some(id => accountModelIdError(id)))
    return t('accounts.modelAccess.invalidModelId')
}

export function accountModelIdError(id: string): string | undefined {
  if (!id || id.trim() !== id || id.startsWith('__') || id.includes('*') || /\p{Cc}/u.test(id) || new TextEncoder().encode(id).length > 256)
    return t('accounts.modelAccess.enterValidExactModelIdUpTo')
}
