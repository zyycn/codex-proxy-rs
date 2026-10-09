import type { Account, ApiKeyConfiguration } from '@/api'

export function isOpenAiApiKeyAccount(account: Pick<Account, 'provider' | 'authenticationKind'> | null | undefined): boolean {
  return account?.provider === 'openai' && account.authenticationKind === 'api_key'
}

export function isOpenAiOAuthAccount(account: Pick<Account, 'provider' | 'authenticationKind'> | null | undefined): boolean {
  return account?.provider === 'openai' && account.authenticationKind === 'oauth'
}

export interface ApiKeyAccountForm extends ApiKeyConfiguration {
  name: string
  apiKey: string
}

export function emptyApiKeyAccountForm(): ApiKeyAccountForm {
  return { name: '', base_url: '', apiKey: '', transport: 'http' }
}

export function parseApiKeyConfiguration(value: Record<string, unknown> | undefined): ApiKeyConfiguration | undefined {
  if (!value || typeof value.base_url !== 'string' || (value.transport !== 'http' && value.transport !== 'prefer_websocket'))
    return undefined
  return { base_url: value.base_url, transport: value.transport }
}

export function apiKeyAccountError(form: ApiKeyAccountForm, editing = false): string | undefined {
  if (!editing && !form.name.trim())
    return '请输入账号名称'
  if (form.base_url.trim().length > 2048)
    return '上游 API 地址不能超过 2048 个字符'
  try {
    const url = new URL(form.base_url)
    if (!['https:', 'http:'].includes(url.protocol) || !url.hostname || url.username || url.password || url.search || url.hash)
      return '请输入不含认证、查询参数或片段的 HTTP 或 HTTPS 地址'
  }
  catch {
    return '请输入完整的上游 API 地址'
  }
  if (!editing && !form.apiKey)
    return '请输入 API Key'
  if (form.apiKey && (!/^[\x21-\x7E]+$/.test(form.apiKey) || form.apiKey.length > 16384))
    return 'API Key 不能包含空格或控制字符'
  return undefined
}
