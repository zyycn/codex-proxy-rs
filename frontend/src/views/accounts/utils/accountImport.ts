import { isRecord } from '@/utils/data'
import { formatProviderLabel, isSupportedProvider } from '@/utils/providers'

export const MAX_ACCOUNT_IMPORT_COUNT = 200
interface AccountImportDocument {
  provider: string
  document: Record<string, unknown>
}

function parseJson(value: string): unknown {
  try {
    return JSON.parse(value)
  }
  catch {
    throw new Error('JSON 格式不正确')
  }
}

export function mixedImportDocuments(text: string): AccountImportDocument[] {
  const value = parseJson(text)
  if (!isRecord(value) || !Array.isArray(value.documents))
    throw new Error('批量导入文件必须是 CPR 多平台导出文件')
  const documents = value.documents.map((entry): AccountImportDocument => {
    if (!isRecord(entry) || typeof entry.provider !== 'string' || !entry.provider.trim() || !isRecord(entry.document))
      throw new Error('批量导入文件包含无效的 Provider 文档')
    return { provider: entry.provider, document: entry.document }
  })
  if (!documents.length)
    throw new Error('批量文件没有可导入的账号文档')
  return documents
}

export function accountImportDocuments(provider: string, mode: string, text: string): AccountImportDocument[] {
  if (provider === 'openai' && (mode === 'access_token' || mode === 'refresh_token')) {
    const tokens = text.split(/\r?\n/).map(token => token.trim()).filter(Boolean)
    const label = mode === 'access_token' ? 'Access Token' : 'Refresh Token'
    if (!tokens.length)
      throw new Error(`请至少粘贴一个 ${label}`)
    const key = mode === 'access_token' ? 'accessToken' : 'refreshToken'
    return tokens.map(token => ({ provider, document: { accounts: [{ [key]: token }] } }))
  }
  const value = parseJson(text)
  // 原生快捷入口保留 CPR 文件展开，插件文档的字段含义由插件自己解释。
  if (isSupportedProvider(provider) && isRecord(value) && Array.isArray(value.documents)) {
    const documents = mixedImportDocuments(text).filter(entry => entry.provider === provider)
    if (!documents.length)
      throw new Error(`批量导入文件不包含 ${formatProviderLabel(provider)} 账号文档`)
    return documents
  }
  if (!isRecord(value))
    throw new Error('导入文件必须是 JSON 对象')
  return [{ provider, document: value }]
}
