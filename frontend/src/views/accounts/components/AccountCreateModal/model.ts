export interface AccountCreateForm {
  provider: string
  name: string
  mode: string
  importText: string
  oauthFlowId: string
  oauthAuthUrl: string
  oauthCallback: string
  proxyMode: string
  proxyUrl: string
}

export function emptyAccountCreateForm(): AccountCreateForm {
  return {
    provider: '',
    name: '',
    mode: 'oauth',
    importText: '',
    oauthFlowId: '',
    oauthAuthUrl: '',
    oauthCallback: '',
    proxyMode: 'direct',
    proxyUrl: '',
  }
}
