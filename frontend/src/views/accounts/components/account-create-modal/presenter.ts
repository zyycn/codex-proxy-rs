import type { AccountAuthorizationView } from '../../composables/useAccountAuthorization'
import type { AccountCreateForm } from '../../utils/accountCreate'
import { t } from '@/i18n'
import { formatProviderLabel } from '@/utils/providers'
import { accountCreateProvider, accountImportModes } from '../../utils/accountCreate'
import { apiKeyAccountError } from '../../utils/upstreamApiKey'

export function resolveAccountCreatePresentation(input: {
  form: AccountCreateForm
  provider: string | undefined
  authorization: AccountAuthorizationView
  callback: string
  busy: boolean
  reauthorizing: boolean
}) {
  const { form, provider, authorization } = input
  const id = accountCreateProvider(form)
  const label = formatProviderLabel(id)
  const isBatch = form.source?.kind === 'bundle'
  const configuring = !input.reauthorizing && form.step === 'settings'
  const modes = accountImportModes(provider)
  const available = isBatch || (form.mode === 'oauth' && Boolean(authorization.flow)) || modes.some(mode => mode.value === form.mode)
  let description = t('accounts.onboarding.jsonDescription')
  if (isBatch)
    description = t('accounts.onboarding.packageDescription')
  else if (form.mode === 'oauth')
    description = t('accounts.onboarding.oauthDescription', { label })
  else if (form.mode === 'api_key')
    description = t('accounts.onboarding.apiKeyDescription')
  else if (form.mode === 'access_token')
    description = t('accounts.onboarding.pasteAccessTokenLineByLineIf')
  else if (form.mode === 'refresh_token')
    description = t('accounts.onboarding.refreshTokenDescription')

  let canSubmit = available && !input.busy
  if (form.mode === 'oauth')
    canSubmit &&= Boolean(authorization.flow && input.callback.trim())
  else if (form.mode === 'api_key')
    canSubmit &&= !apiKeyAccountError(form.apiKey)
  else
    canSubmit &&= Boolean(form.importTexts[form.mode].trim())

  return {
    configuring,
    isBatch,
    provider: id,
    label,
    modeOptions: modes,
    available,
    modal: {
      title: configuring
        ? t('accounts.onboarding.accountSettings')
        : input.reauthorizing
          ? t('accounts.onboarding.reauthorizeAccount')
          : t('accounts.filters.importAccount'),
      description: configuring
        ? t('accounts.onboarding.settingsDescription')
        : input.reauthorizing
          ? t('accounts.onboarding.reauthorizeDescription')
          : description,
      type: configuring ? 'default' as const : 'info' as const,
      width: configuring ? '42rem' : '36rem',
    },
    oauth: {
      callbackLabel: id === 'xai'
        ? t('accounts.onboarding.callbackLabel')
        : t('accounts.onboarding.callbackAddress'),
      callbackPlaceholder: id === 'openai'
        ? 'http://localhost:1455/auth/callback?code=...&state=...'
        : t('accounts.onboarding.callbackPlaceholder'),
      description: t('accounts.onboarding.callbackHelp'),
    },
    importInput: form.mode === 'access_token'
      ? {
          label: 'Access Token',
          placeholder: t('accounts.onboarding.pasteOneAccessTokenPerLine'),
          uploadable: false,
        }
      : form.mode === 'refresh_token'
        ? {
            label: 'Refresh Token',
            placeholder: t('accounts.onboarding.pasteOneRefreshTokenPerLine'),
            uploadable: false,
          }
        : {
            label: isBatch
              ? t('accounts.onboarding.batchAccountFiles')
              : t('accounts.onboarding.accountFile'),
            placeholder: isBatch
              ? t('accounts.onboarding.packagePlaceholder')
              : t('accounts.onboarding.pasteAccountJsonContent'),
            uploadable: true,
          },
    canSubmit,
    submitLabel: form.mode === 'oauth'
      ? input.reauthorizing
        ? t('accounts.onboarding.completeReauthorization')
        : t('accounts.onboarding.completeImport')
      : isBatch
        ? t('accounts.onboarding.batchImport')
        : t('accounts.filters.importAccount'),
  }
}
