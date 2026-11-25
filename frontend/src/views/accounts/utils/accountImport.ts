import type { AccountImportParam } from '@/api'
import { t } from '@/i18n'
import { providerAccounts } from '@/providers/accounts'
import { formatProviderLabel, isBuiltinProvider } from '@/providers/catalog'
import { isRecord } from '@/utils/data'

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
  const split = providerAccounts(entry.provider)?.splitDocument
  return split
    ? split(entry.document).map(document => ({ provider: entry.provider, document }))
    : [entry]
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
  if (mode === 'access_token' || mode === 'refresh_token') {
    const convert = providerAccounts(provider)?.tokenDocuments
    if (!convert)
      throw new Error(t('accounts.onboarding.accountPlatformIsNotSupported'))
    return convert(mode, text).map(document => ({ provider, document }))
  }
  const value = parseJson(text)
  // 原生快捷入口保留 CPR 文件展开，插件文档的字段含义由插件自己解释。
  if (isBuiltinProvider(provider) && isRecord(value) && Array.isArray(value.documents)) {
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
