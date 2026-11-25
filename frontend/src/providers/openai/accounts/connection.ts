import type { Account, AccountConnectionUpdate } from '@/api'

export interface ApiKeyConfiguration {
  base_url: string
  protocol: NonNullable<AccountConnectionUpdate['protocol']>
  transport: AccountConnectionUpdate['transport']
}

export function connectionKind(account: Account | null) {
  if (account?.provider === 'openai' && (account.authenticationKind === 'api_key' || account.authenticationKind === 'oauth'))
    return account.authenticationKind
}

export interface ApiKeyAccountForm extends Omit<ApiKeyConfiguration, 'transport'> {
  apiKey: string
  transport: ApiKeyConfiguration['transport'] | ''
}

export function emptyApiKeyAccountForm(): ApiKeyAccountForm {
  return {
    base_url: '',
    apiKey: '',
    protocol: 'responses',
    transport: 'http',
  }
}

export function parseApiKeyConfiguration(value: Record<string, unknown> | undefined): ApiKeyConfiguration | undefined {
  if (!value || typeof value.base_url !== 'string' || (value.transport !== 'http' && value.transport !== 'prefer_websocket'))
    return undefined
  if (value.protocol !== 'responses' && value.protocol !== 'messages')
    return undefined
  if (value.protocol === 'messages' && value.transport !== 'http')
    return undefined
  return { base_url: value.base_url, protocol: value.protocol, transport: value.transport }
}
