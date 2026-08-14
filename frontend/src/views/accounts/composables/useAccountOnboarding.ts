import type { getAccounts } from '@/api'

import { computed, ref, shallowRef, watch } from 'vue'
import {
  completeAccountOAuth,
  importAccounts,
  startAccountOAuth,
} from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { providerDisplayName } from '@/utils/providers'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]
type ImportProvider = 'openai' | 'xai'

interface MixedImportDocument {
  provider: ImportProvider
  document: Record<string, unknown>
}

export function useAccountOnboarding(options: {
  reload: () => Promise<unknown>
}) {
  const createModalOpen = shallowRef(false)
  const reauthorizingAccount = shallowRef<AccountRow | null>(null)
  const creatingAccountAction = useAsyncAction()
  const authorizingOAuthAction = useAsyncAction()
  const creatingAccount = creatingAccountAction.loading
  const authorizingOAuth = authorizingOAuthAction.loading
  const createForm = ref(emptyCreateForm())

  const showCreateModal = computed({
    get: () => createModalOpen.value,
    set: (value: boolean) => {
      createModalOpen.value = value
      if (!value) {
        reauthorizingAccount.value = null
        createForm.value = emptyCreateForm()
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
        const input = await newAccountInput()
        const account = reauthorizingAccount.value
        const result = await startAccountOAuth({
          ...input,
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
    createForm.value = emptyCreateForm()
    showCreateModal.value = true
  }

  function openReauthorizeAccount(account: AccountRow) {
    if (account.provider !== 'openai' && account.provider !== 'xai')
      return
    reauthorizingAccount.value = account
    createForm.value = {
      ...emptyCreateForm(),
      provider: account.provider,
      name: account.name,
      mode: 'oauth',
    }
    showCreateModal.value = true
    void handleAuthorizeOAuth()
  }

  async function newAccountInput() {
    const account = reauthorizingAccount.value
    return {
      provider: createForm.value.provider,
      name: account?.name || account?.email || `${createForm.value.provider} OAuth`,
    }
  }

  async function importAccountDocument() {
    const provider = requireImportProvider(createForm.value.provider)
    const documents = providerImportDocuments(
      parseImportJson(createForm.value.importText),
      provider,
    )
    let importedCount = 0
    for (const entry of documents) {
      const result = await importAccounts({
        provider,
        data: entry.document,
      })
      importedCount += result.importedCount
    }
    return `${providerDisplayName(provider) ?? provider} 账号已导入 ${importedCount} 个`
  }

  async function importMixedAccountDocument() {
    const documents = parseMixedImportDocuments(parseImportJson(createForm.value.importText))
    let importedCount = 0
    const failures: string[] = []

    for (const entry of documents) {
      try {
        const result = await importAccounts({
          provider: entry.provider,
          data: entry.document,
        })
        importedCount += result.importedCount
      }
      catch (error) {
        const message = error instanceof Error && error.message
          ? error.message
          : '导入失败'
        failures.push(`${providerDisplayName(entry.provider) ?? 'OpenAI'}：${message}`)
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
        oauthFlowId: '',
        oauthAuthUrl: '',
        oauthCallback: '',
      }
    },
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

function emptyCreateForm() {
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

export function accountProviderModeOptions(provider: string) {
  if (provider === 'xai') {
    return [
      { label: 'OAuth', value: 'oauth' },
      { label: '账号文件', value: 'json' },
    ]
  }
  if (provider !== 'openai')
    return []
  return [
    { label: 'OAuth', value: 'oauth' },
    { label: '账号文件', value: 'json' },
    { label: 'Agent 身份', value: 'agent_identity' },
  ]
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
  if (value === 'openai' || value === 'xai')
    return value
  throw new Error('请选择要导入的账号平台')
}

function providerImportDocuments(value: unknown, provider: ImportProvider): MixedImportDocument[] {
  if (isRecord(value) && Array.isArray(value.documents)) {
    const documents = parseMixedImportDocuments(value)
      .filter(entry => entry.provider === provider)
    if (documents.length === 0) {
      const label = providerDisplayName(provider) ?? provider
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
    if (provider !== 'openai' && provider !== 'xai')
      throw new Error('批量导入文件包含无效的 Provider 文档')
    if (!isRecord(entry.document))
      throw new Error('批量导入文件包含无效的 Provider 文档')
    documents.push({ provider, document: entry.document })
  }

  if (documents.length === 0)
    throw new Error('批量文件没有可导入的账号文档')
  return documents
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}
