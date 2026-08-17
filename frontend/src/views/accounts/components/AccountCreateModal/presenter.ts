import type { AccountRow } from '../../constants'
import type { AccountCreateForm } from './model'

export type AccountCreateProvider = 'batch' | 'openai' | 'xai'

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
    { label: '账号文件', value: 'json' },
    { label: 'Agent 身份', value: 'agent_identity' },
  ],
  xai: [
    { label: 'OAuth', value: 'oauth' },
    { label: '账号文件', value: 'json' },
  ],
} satisfies Record<Exclude<AccountCreateProvider, 'batch'>, Array<{ label: string, value: string }>>

export function resolveAccountCreatePresentation(input: AccountCreatePresentationInput) {
  const { form } = input
  const providerSelected = isAccountCreateProvider(form.provider)
  const choosingProvider = !input.reauthorizing && !providerSelected
  const isXai = form.provider === 'xai'
  const isBatch = form.provider === 'batch'
  const oauthAuthUrl = form.oauthAuthUrl || ''
  const accountName = input.account?.email
    || input.account?.accountId
    || input.account?.id
    || '该账号'

  return {
    choosingProvider,
    isXai,
    isBatch,
    modeOptions: form.provider === 'openai' || form.provider === 'xai'
      ? modeOptions[form.provider]
      : [],
    modal: {
      title: input.reauthorizing
        ? '重新授权账号'
        : choosingProvider ? '选择账号平台' : '导入账号',
      description: modalDescription(input, { isBatch, isXai, choosingProvider }),
      tone: choosingProvider ? 'neutral' as const : 'info' as const,
      size: choosingProvider ? 'sm' as const : 'md' as const,
    },
    oauth: {
      authUrl: oauthAuthUrl,
      panelTitle: input.reauthorizing
        ? accountName
        : isXai ? 'xAI OAuth 授权' : 'OpenAI OAuth 授权',
      panelDescription: isXai
        ? '生成并打开授权链接 → 完成浏览器授权 → 粘贴回调地址、查询字符串或授权码'
        : '生成并打开授权链接 → 完成浏览器授权 → 粘贴回调地址',
      callbackLabel: isXai ? '回调地址或授权码' : '回调地址',
      callbackPlaceholder: isXai
        ? '回调地址、?code=...&state=... 或授权码'
        : 'http://localhost:1455/auth/callback?code=...&state=...',
    },
    importFile: {
      label: isBatch
        ? '批量账号文件'
        : form.mode === 'agent_identity' ? 'Agent 身份文件' : '账号文件',
      placeholder: form.mode === 'agent_identity'
        ? '粘贴 Agent 身份文件内容'
        : isBatch
          ? '粘贴 CPR 多平台导出文件内容'
          : '粘贴 CPR、Sub2API 或 CPA 账号文件内容',
    },
    canGenerateOauth: providerSelected && !input.saving && !input.oauthLoading,
    canSubmit: canSubmit(input, providerSelected, oauthAuthUrl),
    submitLabel: input.reauthorizing
      ? '完成重新授权'
      : form.mode === 'oauth' ? '完成授权导入' : isBatch ? '批量导入' : '导入',
  }
}

function modalDescription(
  input: AccountCreatePresentationInput,
  state: { isBatch: boolean, isXai: boolean, choosingProvider: boolean },
) {
  if (state.choosingProvider)
    return undefined
  if (input.reauthorizing) {
    return state.isXai
      ? '完成新的 xAI 授权并替换此账号凭据'
      : '完成新的 OpenAI 授权并替换此账号凭据'
  }
  if (state.isBatch)
    return '粘贴或上传 CPR 账号包，一次导入多个平台账号'
  if (state.isXai) {
    return input.form.mode === 'oauth'
      ? '通过浏览器授权导入 xAI 账号'
      : '粘贴或上传 xAI 账号文件，匹配已有账号时更新凭据'
  }
  if (input.form.mode === 'oauth')
    return '通过浏览器授权导入 OpenAI 账号'
  if (input.form.mode === 'agent_identity')
    return '粘贴或上传 Agent 身份文件，匹配已有账号时更新凭据'
  return '粘贴或上传 CPR、Sub2API 或 CPA 账号文件，匹配已有账号时更新凭据'
}

function canSubmit(
  input: AccountCreatePresentationInput,
  providerSelected: boolean,
  oauthAuthUrl: string,
) {
  if (!providerSelected || input.saving || input.oauthLoading)
    return false
  if (input.form.mode !== 'oauth')
    return input.form.importText.trim().length > 0
  return Boolean(
    input.form.oauthFlowId
    && oauthAuthUrl
    && input.form.oauthCallback.trim(),
  )
}

function isAccountCreateProvider(value: string): value is AccountCreateProvider {
  return value === 'openai' || value === 'xai' || value === 'batch'
}
