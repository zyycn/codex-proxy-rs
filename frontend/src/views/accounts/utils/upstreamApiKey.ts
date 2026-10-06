import type { Account, ApiKeyConfiguration } from '@/api'
import { parseWebsocketMaxAgeSeconds } from './websocketAge'

export function isOpenAiApiKeyAccount(account: Pick<Account, 'provider' | 'authenticationKind'> | null | undefined): boolean {
  return account?.provider === 'openai' && account.authenticationKind === 'api_key'
}

export function isOpenAiOAuthAccount(account: Pick<Account, 'provider' | 'authenticationKind'> | null | undefined): boolean {
  return account?.provider === 'openai' && account.authenticationKind === 'oauth'
}

export interface ApiKeyAccountForm extends Omit<ApiKeyConfiguration, 'websocket_max_age_ms'> {
  name: string
  apiKey: string
  websocketMaxAgeSeconds: string
}

export function emptyApiKeyAccountForm(): ApiKeyAccountForm {
  return { name: '', base_url: '', apiKey: '', transport: 'http', websocketMaxAgeSeconds: '' }
}

export function parseOpenAiConnectionConfiguration(value: Record<string, unknown> | undefined): Omit<ApiKeyConfiguration, 'base_url'> | undefined {
  if (!value || (value.transport !== 'http' && value.transport !== 'prefer_websocket'))
    return undefined
  const age = value.websocket_max_age_ms
  if (age !== undefined && (typeof age !== 'number' || !Number.isSafeInteger(age) || age <= 0))
    return undefined
  return { transport: value.transport, websocket_max_age_ms: age }
}

export function parseApiKeyConfiguration(value: Record<string, unknown> | undefined): ApiKeyConfiguration | undefined {
  const connection = parseOpenAiConnectionConfiguration(value)
  if (!connection || typeof value?.base_url !== 'string')
    return undefined
  return { ...connection, base_url: value.base_url }
}

export function apiKeyAccountError(form: ApiKeyAccountForm, editing = false): string | undefined {
  if (!editing && !form.name.trim())
    return '请输入账号名称'
  if (form.base_url.trim().length > 2048)
    return '上游 API 地址不能超过 2048 个字符'
  try {
    const url = new URL(form.base_url)
    const loopback = url.hostname === 'localhost' || url.hostname === '[::1]' || /^127(?:\.\d{1,3}){3}$/.test(url.hostname)
    if ((url.protocol !== 'https:' && !(url.protocol === 'http:' && loopback)) || url.username || url.password || url.search || url.hash)
      return '请输入不含认证、查询参数或片段的 HTTPS 地址'
  }
  catch {
    return '请输入完整的上游 API 地址'
  }
  if (!editing && !form.apiKey)
    return '请输入 API Key'
  if (form.apiKey && (!/^[\x21-\x7E]+$/.test(form.apiKey) || form.apiKey.length > 16384))
    return 'API Key 不能包含空格或控制字符'
  if (form.transport === 'prefer_websocket') {
    const age = parseWebsocketMaxAgeSeconds(form.websocketMaxAgeSeconds)
    if (!age.valid)
      return age.message
  }
  return undefined
}
