import { parseAccountSchedulingForm } from '../../utils/schedulingForm'

export type AccountCreateProvider = 'batch' | 'openai' | 'xai'
export type AccountImportMode = 'oauth' | 'access_token' | 'refresh_token' | 'json'
export type AccountImportInputMode = Exclude<AccountImportMode, 'oauth'>

export interface AccountCreateForm {
  provider: AccountCreateProvider | ''
  enabled: boolean
  concurrencyLimit: string
  weight: string
  groupIds: string[]
  step: 'settings' | 'import'
  mode: AccountImportMode
  importTexts: Record<AccountImportInputMode, string>
  oauthFlowId: string
  oauthAuthUrl: string
  oauthCallback: string
  proxyMode: string
  proxyId: string
}

export function emptyAccountCreateForm(): AccountCreateForm {
  return {
    provider: '',
    enabled: true,
    concurrencyLimit: '',
    weight: '1',
    groupIds: [],
    step: 'settings',
    mode: 'oauth',
    importTexts: { access_token: '', refresh_token: '', json: '' },
    oauthFlowId: '',
    oauthAuthUrl: '',
    oauthCallback: '',
    proxyMode: 'direct',
    proxyId: '',
  }
}

export function accountProxyError(form: AccountCreateForm): string | undefined {
  if (form.proxyMode !== 'proxy')
    return undefined
  if (!form.proxyId.trim())
    return '请选择已通过测试的代理'
  return undefined
}

export function accountImportSettings(form: AccountCreateForm) {
  const scheduling = parseAccountSchedulingForm(form.concurrencyLimit, form.weight)
  if (!scheduling.valid)
    throw new Error(scheduling.message)
  return { enabled: form.enabled, ...scheduling.values, groupIds: [...new Set(form.groupIds)] }
}
