import type { AccountImportParam } from '@/api'
import { t } from '@/i18n'
import { isRecord } from '@/utils/data'
import { formatProviderLabel, isSupportedProvider } from '@/utils/providers'

const MAX_ACCOUNT_IMPORT_COUNT = 200
const MAX_ACCOUNT_IMPORT_BYTES = 4 * 1024 * 1024
interface AccountImportDocument {
  provider: string
  document: Record<string, unknown>
}

function parseJson(value: string): unknown {
  try {
    return JSON.parse(value)
  }
  catch {
    throw new Error(t('accounts.import.jsonFormatIsIncorrect'))
  }
}

function splitAccountDocuments(entry: AccountImportDocument): AccountImportDocument[] {
  // xAI 的整份文档承担 RT 去重，必须保留在 Provider 的刷新边界内
  if (entry.provider !== 'openai')
    return [entry]
  const document = entry.document
  const data = isRecord(document.data) && Array.isArray(document.data.accounts)
    ? document.data
    : document
  if (!Array.isArray(data.accounts) || !data.accounts.length)
    return [entry]
  return data.accounts.map((account) => {
    const item: Record<string, unknown> = {
      ...data,
      accounts: [account],
    }
    if (Array.isArray(data.proxies)) {
      // 保留同键的全部记录，让 Provider 继续拒绝重复引用，不复制其他账号的代理
      const key = isRecord(account)
        ? account.proxy_key
        : undefined
      item.proxies = data.proxies.filter(proxy => typeof key === 'string' && isRecord(proxy) && proxy.proxy_key === key)
    }
    return {
      provider: entry.provider,
      document: data === document
        ? item
        : { ...document, data: item },
    }
  })
}

export function accountImportBatches(items: AccountImportParam[]): AccountImportParam[][] {
  const encoder = new TextEncoder()
  // UUID 固定 36 字符，计入信封及数组逗号后与实际 JSON 请求体使用同一字节上限
  const envelopeBytes = encoder.encode(JSON.stringify({ submissionId: '00000000-0000-0000-0000-000000000000', items: [] })).length
  const batches: AccountImportParam[][] = []
  let batch: AccountImportParam[] = []
  let bytes = envelopeBytes
  for (const item of items) {
    const itemBytes = encoder.encode(JSON.stringify(item)).length
    if (envelopeBytes + itemBytes > MAX_ACCOUNT_IMPORT_BYTES)
      throw new Error(t('accounts.import.documentTooLarge'))
    if (batch.length >= MAX_ACCOUNT_IMPORT_COUNT || bytes + itemBytes + Number(batch.length > 0) > MAX_ACCOUNT_IMPORT_BYTES) {
      batches.push(batch)
      batch = []
      bytes = envelopeBytes
    }
    bytes += itemBytes + Number(batch.length > 0)
    batch.push(item)
  }
  if (batch.length)
    batches.push(batch)
  return batches
}

export function mixedImportDocuments(text: string): AccountImportDocument[] {
  const value = parseJson(text)
  if (!isRecord(value) || !Array.isArray(value.documents))
    throw new Error(t('accounts.import.batchImportFileMustBeCprMulti'))
  const documents = value.documents.flatMap((entry): AccountImportDocument[] => {
    if (!isRecord(entry) || typeof entry.provider !== 'string' || !entry.provider.trim() || !isRecord(entry.document))
      throw new Error(t('accounts.import.invalidProvider'))
    return splitAccountDocuments({ provider: entry.provider, document: entry.document })
  })
  if (!documents.length)
    throw new Error(t('accounts.import.noAccounts'))
  return documents
}

export function accountImportDocuments(provider: string, mode: string, text: string): AccountImportDocument[] {
  if (provider === 'openai' && (mode === 'access_token' || mode === 'refresh_token')) {
    const tokens = text.split(/\r?\n/).map(token => token.trim()).filter(Boolean)
    const label = mode === 'access_token'
      ? 'Access Token'
      : 'Refresh Token'
    if (!tokens.length)
      throw new Error(t('accounts.import.pasteAtLeastOne', { label }))
    const key = mode === 'access_token'
      ? 'accessToken'
      : 'refreshToken'
    return tokens.map(token => ({
      provider,
      document: {
        accounts: [
          { [key]: token },
        ],
      },
    }))
  }
  const value = parseJson(text)
  // 原生快捷入口保留 CPR 文件展开，插件文档的字段含义由插件自己解释。
  if (isSupportedProvider(provider) && isRecord(value) && Array.isArray(value.documents)) {
    const documents = mixedImportDocuments(text).filter(entry => entry.provider === provider)
    if (!documents.length) {
      throw new Error(t('accounts.import.missingProvider', {
        provider: formatProviderLabel(provider),
      }))
    }
    return documents
  }
  if (!isRecord(value))
    throw new Error(t('accounts.import.importFilesMustBeJsonObjects'))
  return splitAccountDocuments({ provider, document: value })
}
