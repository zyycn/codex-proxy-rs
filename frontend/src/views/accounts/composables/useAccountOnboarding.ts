import type { getAccounts } from '@/api'

import { computed, ref, shallowRef, watch } from 'vue'
import {
  completeAccountOAuth,
  importAccounts,
  startAccountOAuth,
} from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { errorMessage } from '@/utils/async'
import { isRecord } from '@/utils/object'
import { formatProviderLabel, isSupportedProvider } from '@/utils/providers'
import { accountImportSettings, accountProxyError, emptyAccountCreateForm } from '../components/AccountCreateModal/model'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]
type ImportProvider = 'openai' | 'xai'

interface MixedImportDocument {
  provider: ImportProvider
  document: Record<string, unknown>
}

type OpenAiTokenImportMode = 'access_token' | 'refresh_token'

const MAX_TOKEN_IMPORT_COUNT = 200

export function useAccountOnboarding(options: {
  reload: () => Promise<unknown>
}) {
  const createModalOpen = shallowRef(false)
  const reauthorizingAccount = shallowRef<AccountRow | null>(null)
  const creatingAccountAction = useAsyncAction()
  const authorizingOAuthAction = useAsyncAction()
  const creatingAccount = creatingAccountAction.loading
  const authorizingOAuth = authorizingOAuthAction.loading
  const createForm = ref(emptyAccountCreateForm())

  const showCreateModal = computed({
    get: () => createModalOpen.value,
    set: (value: boolean) => {
      createModalOpen.value = value
      if (!value) {
        reauthorizingAccount.value = null
        createForm.value = emptyAccountCreateForm()
      }
    },
  })

  async function handleCreate() {
    if (createForm.value.mode === 'oauth') {
      await completeOAuth()
      return
    }
    if (creatingAccount.value)
      return

    await creatingAccountAction.run(
      async () => {
        const proxyError = accountProxyError(createForm.value)
        if (proxyError)
          throw new Error(proxyError)
        const message = createForm.value.provider === 'batch'
          ? await importMixedAccountDocument()
          : await importAccountDocument()
        await finishCreate(message)
      },
      { errorText: '导入失败' },
    )
  }

  async function handleAuthorizeOAuth() {
    if (authorizingOAuth.value)
      return

    await authorizingOAuthAction.run(
      async () => {
        const input = newAccountInput()
        const account = reauthorizingAccount.value
        const proxyError = accountProxyError(createForm.value)
        if (!account && proxyError)
          throw new Error(proxyError)
        const result = await startAccountOAuth({
          ...input,
          outboundProxyUrl: !account && createForm.value.proxyMode === 'proxy' ? createForm.value.proxyUrl.trim() : undefined,
          ...(account
            ? {
                accountId: account.id,
              }
            : {}),
        })

        createForm.value = {
          ...createForm.value,
          oauthFlowId: result.flowId,
          oauthAuthUrl: result.authorizationUrl,
          oauthCallback: '',
        }
        toast.success('授权链接已生成')
      },
      { errorText: '授权链接生成失败' },
    )
  }

  async function completeOAuth() {
    if (creatingAccount.value)
      return

    await creatingAccountAction.run(
      async () => {
        if (!createForm.value.oauthFlowId)
          throw new Error('请先生成授权链接')

        const callbackUrl = createForm.value.oauthCallback.trim()
        if (!callbackUrl) {
          throw new Error(createForm.value.provider === 'xai'
            ? '请粘贴 OAuth 回调地址、含 code 和 state 的查询字符串或授权码'
            : '请粘贴 OAuth 回调地址')
        }
        await completeAccountOAuth({
          provider: createForm.value.provider,
          flowId: createForm.value.oauthFlowId,
          callbackUrl,
          settings: reauthorizingAccount.value ? undefined : accountImportSettings(createForm.value),
        })
        await finishCreate(
          reauthorizingAccount.value
            ? '账号重新授权成功'
            : createForm.value.provider === 'xai'
              ? 'xAI OAuth 账号已添加'
              : 'OpenAI OAuth 账号已添加',
        )
      },
      {
        errorText: reauthorizingAccount.value ? '重新授权失败' : 'OAuth 授权导入失败',
      },
    )
  }

  function openCreateAccount() {
    reauthorizingAccount.value = null
    createForm.value = emptyAccountCreateForm()
    showCreateModal.value = true
  }

  function openReauthorizeAccount(account: AccountRow) {
    if (account.provider !== 'openai' && account.provider !== 'xai')
      return
    reauthorizingAccount.value = account
    createForm.value = {
      ...emptyAccountCreateForm(),
      provider: account.provider,
      step: 'import',
      mode: 'oauth',
    }
    showCreateModal.value = true
    void handleAuthorizeOAuth()
  }

  function newAccountInput() {
    const account = reauthorizingAccount.value
    return {
      provider: createForm.value.provider,
      name: account?.name || account?.email || `${createForm.value.provider} OAuth`,
    }
  }

  async function importAccountDocument() {
    const provider = requireImportProvider(createForm.value.provider)
    const mode = createForm.value.mode
    if (mode === 'oauth')
      throw new Error('请选择凭据导入方式')
    const documents = accountImportDocuments(
      provider,
      mode,
      createForm.value.importTexts[mode],
    )
    let importedCount = 0
    for (const entry of documents) {
      const result = await importAccounts({
        provider,
        settings: accountImportSettings(createForm.value),
        data: createForm.value.proxyMode === 'proxy'
          ? withDefaultImportProxy(entry.document, createForm.value.proxyUrl.trim())
          : entry.document,
      })
      importedCount += result.importedCount
    }
    return `${formatProviderLabel(provider)} 账号已导入 ${importedCount} 个`
  }

  async function importMixedAccountDocument() {
    const documents = parseMixedImportDocuments(parseImportJson(createForm.value.importTexts.json))
    let importedCount = 0
    const failures: string[] = []

    for (const entry of documents) {
      try {
        const result = await importAccounts({
          provider: entry.provider,
          settings: accountImportSettings(createForm.value),
          data: createForm.value.proxyMode === 'proxy'
            ? withDefaultImportProxy(entry.document, createForm.value.proxyUrl.trim())
            : entry.document,
        })
        importedCount += result.importedCount
      }
      catch (error) {
        const message = errorMessage(error, '导入失败')
        failures.push(`${formatProviderLabel(entry.provider, 'OpenAI')}：${message}`)
      }
    }

    if (importedCount === 0) {
      throw new Error(failures.length > 0 ? `批量导入失败：${failures.join('、')}` : '批量文件没有可导入的账号')
    }

    if (failures.length > 0) {
      return `已导入 ${importedCount} 个账号，${failures.length} 个文档失败：${failures.join('、')}`
    }
    return `已导入 ${importedCount} 个账号`
  }

  async function finishCreate(message: string) {
    showCreateModal.value = false
    await options.reload()
    toast.success(message)
  }

  watch(
    () => createForm.value.provider,
    () => {
      createForm.value = {
        ...createForm.value,
        mode: createForm.value.provider === 'batch' ? 'json' : 'oauth',
        importTexts: { access_token: '', refresh_token: '', json: '' },
        oauthFlowId: '',
        oauthAuthUrl: '',
        oauthCallback: '',
      }
    },
    { flush: 'sync' },
  )

  watch(
    [
      () => createForm.value.proxyMode,
      () => createForm.value.proxyMode === 'proxy' ? createForm.value.proxyUrl.trim() : '',
    ],
    () => {
      createForm.value.oauthFlowId = ''
      createForm.value.oauthAuthUrl = ''
      createForm.value.oauthCallback = ''
    },
    { flush: 'sync' },
  )

  return {
    showCreateModal,
    reauthorizingAccount,
    creatingAccount,
    authorizingOAuth,
    createForm,
    handleCreate,
    handleAuthorizeOAuth,
    openCreateAccount,
    openReauthorizeAccount,
  }
}

function withDefaultImportProxy(document: Record<string, unknown>, proxyUrl: string): Record<string, unknown> {
  if (isRecord(document.data) && Array.isArray(document.data.accounts))
    return { ...document, data: withDefaultImportProxy(document.data, proxyUrl) }
  if (Array.isArray(document.accounts)) {
    return {
      ...document,
      accounts: document.accounts.map(account => isRecord(account) ? withDefaultImportProxy(account, proxyUrl) : account),
    }
  }
  if (['outboundProxyUrl', 'outbound_proxy_url', 'proxy_key'].some(key => Object.hasOwn(document, key)))
    return document
  return { ...document, outboundProxyUrl: proxyUrl }
}

function parseImportJson(value: string) {
  try {
    return JSON.parse(value)
  }
  catch {
    throw new Error('JSON 格式不正确')
  }
}

function requireImportProvider(value: string): ImportProvider {
  if (isSupportedProvider(value))
    return value
  throw new Error('请选择要导入的账号平台')
}

function accountImportDocuments(
  provider: ImportProvider,
  mode: string,
  value: string,
): MixedImportDocument[] {
  if (provider === 'openai' && isOpenAiTokenImportMode(mode)) {
    return [{
      provider,
      document: parseOpenAiTokenImport(value, mode),
    }]
  }
  return providerImportDocuments(parseImportJson(value), provider)
}

function parseOpenAiTokenImport(value: string, mode: OpenAiTokenImportMode) {
  const tokens = value
    .split(/\r?\n/)
    .map(token => token.trim())
    .filter(Boolean)
  const label = mode === 'access_token' ? 'Access Token' : 'Refresh Token'

  if (tokens.length === 0)
    throw new Error(`请至少粘贴一个 ${label}`)
  if (tokens.length > MAX_TOKEN_IMPORT_COUNT)
    throw new Error(`单次最多导入 ${MAX_TOKEN_IMPORT_COUNT} 个 ${label}`)

  const credentialKey = mode === 'access_token' ? 'accessToken' : 'refreshToken'
  return {
    accounts: tokens.map(token => ({ [credentialKey]: token })),
  }
}

function isOpenAiTokenImportMode(value: string): value is OpenAiTokenImportMode {
  return value === 'access_token' || value === 'refresh_token'
}

function providerImportDocuments(value: unknown, provider: ImportProvider): MixedImportDocument[] {
  if (isRecord(value) && Array.isArray(value.documents)) {
    const documents = parseMixedImportDocuments(value)
      .filter(entry => entry.provider === provider)
    if (documents.length === 0) {
      const label = formatProviderLabel(provider)
      throw new Error(`批量导入文件不包含 ${label} 账号文档`)
    }
    return documents
  }
  if (!isRecord(value))
    throw new Error('导入文件必须是 JSON object')
  return [{ provider, document: value }]
}

function parseMixedImportDocuments(value: unknown): MixedImportDocument[] {
  if (!isRecord(value) || !Array.isArray(value.documents))
    throw new Error('批量导入文件必须是 CPR 多平台导出文件')

  const documents: MixedImportDocument[] = []
  for (const entry of value.documents) {
    if (!isRecord(entry))
      throw new Error('批量导入文件包含无效的 Provider 文档')
    const provider = entry.provider
    if (!isSupportedProvider(provider))
      throw new Error('批量导入文件包含无效的 Provider 文档')
    if (!isRecord(entry.document))
      throw new Error('批量导入文件包含无效的 Provider 文档')
    documents.push({ provider, document: entry.document })
  }

  if (documents.length === 0)
    throw new Error('批量文件没有可导入的账号文档')
  return documents
}
