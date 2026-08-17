export interface AccountCreateForm {
  provider: string
  name: string
  mode: string
  importText: string
  oauthFlowId: string
  oauthAuthUrl: string
  oauthCallback: string
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
  }
}
