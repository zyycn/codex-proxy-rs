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
  proxyUrl: string
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
    proxyUrl: '',
  }
}

export function accountProxyError(form: AccountCreateForm): string | undefined {
  if (form.proxyMode !== 'proxy')
    return undefined
  if (!form.proxyUrl.trim())
    return '请输入代理 URL'
  try {
    const url = new URL(form.proxyUrl.trim())
    if (!['http:', 'https:', 'socks5:', 'socks5h:'].includes(url.protocol) || !url.hostname)
      return '请输入 HTTP、HTTPS 或 SOCKS5 代理地址'
    if (url.protocol.startsWith('socks5') && !url.port)
      return 'SOCKS5 代理地址需要填写端口'
  }
  catch {
    return '请输入有效的代理 URL'
  }
  return undefined
}

export function accountImportSettings(form: AccountCreateForm) {
  const scheduling = parseAccountSchedulingForm(form.concurrencyLimit, form.weight)
  if (!scheduling.valid)
    throw new Error(scheduling.message)
  return { enabled: form.enabled, ...scheduling.values, groupIds: [...new Set(form.groupIds)] }
}
