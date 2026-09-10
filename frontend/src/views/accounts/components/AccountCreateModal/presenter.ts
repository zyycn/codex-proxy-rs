import type { AccountRow } from '../../constants'
import type { AccountCreateForm, AccountCreateProvider } from './model'

interface AccountCreatePresentationInput {
  form: AccountCreateForm
  account: AccountRow | null
  saving: boolean
  oauthLoading: boolean
  reauthorizing: boolean
}

const modeOptions = {
  openai: [
    { label: 'OAuth', value: 'oauth' },
    { label: 'AT', value: 'access_token' },
    { label: 'RT', value: 'refresh_token' },
    { label: '账号文件', value: 'json' },
  ],
  xai: [
    { label: 'OAuth', value: 'oauth' },
    { label: '账号文件', value: 'json' },
  ],
} satisfies Record<Exclude<AccountCreateProvider, 'batch'>, Array<{ label: string, value: string }>>

export function resolveAccountCreatePresentation(input: AccountCreatePresentationInput) {
  const { form } = input
  const provider = isAccountCreateProvider(form.provider) ? form.provider : undefined
  const configuring = !input.reauthorizing && form.step === 'settings'
  const oauthAuthUrl = form.oauthAuthUrl || ''

  return {
    configuring,
    isXai: provider === 'xai',
    isBatch: provider === 'batch',
    modeOptions: resolveModeOptions(provider),
    modal: resolveModal(input, provider, configuring),
    oauth: resolveOAuth(provider, oauthAuthUrl),
    importInput: resolveImportInput(form, provider),
    canSubmit: canSubmit(input, provider, oauthAuthUrl),
    submitLabel: resolveSubmitLabel(input, provider),
  }
}

function resolveModeOptions(provider: AccountCreateProvider | undefined) {
  if (provider === 'openai' || provider === 'xai')
    return modeOptions[provider]
  return []
}

function resolveModal(
  input: AccountCreatePresentationInput,
  provider: AccountCreateProvider | undefined,
  configuring: boolean,
) {
  if (configuring) {
    return {
      title: '账号设置',
      description: '设置将应用于本次导入的账号',
      tone: 'neutral' as const,
      size: 'md' as const,
    }
  }

  if (input.reauthorizing) {
    const providerName = provider === 'xai' ? 'xAI' : 'OpenAI'
    return {
      title: '重新授权账号',
      description: `${input.account?.email || input.account?.name || providerName} · 完成授权后更新账号凭据`,
      tone: 'info' as const,
      size: 'md' as const,
    }
  }

  let description = '粘贴或上传包含 OAuth Token 的 JSON，匹配已有账号时更新凭据'
  if (provider === 'batch')
    description = '粘贴或上传 CPR 账号包，一次导入多个平台账号'
  else if (provider === 'xai' && input.form.mode === 'oauth')
    description = '通过浏览器授权导入 xAI 账号'
  else if (provider === 'xai')
    description = '粘贴或上传 xAI 账号文件，匹配已有账号时更新凭据'
  else if (input.form.mode === 'oauth')
    description = '通过浏览器授权导入 OpenAI 账号'
  else if (input.form.mode === 'access_token')
    description = '逐行粘贴 Access Token；未包含 Refresh Token 时无法自动续期'
  else if (input.form.mode === 'refresh_token')
    description = '逐行粘贴 Refresh Token，导入时将自动换取 Access Token'

  return {
    title: '导入账号',
    description,
    tone: 'info' as const,
    size: 'md' as const,
  }
}

function resolveOAuth(provider: AccountCreateProvider | undefined, authUrl: string) {
  if (provider === 'xai') {
    return {
      authUrl,
      callbackLabel: '回调地址或授权码',
      callbackPlaceholder: '回调地址、?code=...&state=... 或授权码',
    }
  }

  return {
    authUrl,
    callbackLabel: '回调地址',
    callbackPlaceholder: 'http://localhost:1455/auth/callback?code=...&state=...',
  }
}

function resolveImportInput(
  form: AccountCreateForm,
  provider: AccountCreateProvider | undefined,
) {
  if (form.mode === 'access_token') {
    return {
      label: 'Access Token',
      placeholder: '每行粘贴一个 Access Token',
      uploadable: false,
    }
  }
  if (form.mode === 'refresh_token') {
    return {
      label: 'Refresh Token',
      placeholder: '每行粘贴一个 Refresh Token',
      uploadable: false,
    }
  }
  if (provider === 'batch') {
    return {
      label: '批量账号文件',
      placeholder: '粘贴 CPR 多平台导出文件内容',
      uploadable: true,
    }
  }
  if (provider === 'xai') {
    return {
      label: '账号文件',
      placeholder: '粘贴包含 xAI OAuth 凭据的 JSON 内容',
      uploadable: true,
    }
  }
  return {
    label: '账号文件',
    placeholder: '粘贴包含 accessToken 或 refreshToken（可含 idToken）的 JSON 内容',
    uploadable: true,
  }
}

function resolveSubmitLabel(
  input: AccountCreatePresentationInput,
  provider: AccountCreateProvider | undefined,
) {
  if (input.reauthorizing)
    return '完成重新授权'
  if (input.form.mode === 'oauth')
    return '完成导入'
  if (provider === 'batch')
    return '批量导入'
  return '导入账号'
}

function canSubmit(
  input: AccountCreatePresentationInput,
  provider: AccountCreateProvider | undefined,
  oauthAuthUrl: string,
) {
  if (!provider || input.saving || input.oauthLoading)
    return false
  if (input.form.mode !== 'oauth')
    return input.form.importTexts[input.form.mode].trim().length > 0
  return Boolean(
    input.form.oauthFlowId
    && oauthAuthUrl
    && input.form.oauthCallback.trim(),
  )
}

function isAccountCreateProvider(value: string): value is AccountCreateProvider {
  return value === 'openai' || value === 'xai' || value === 'batch'
}
