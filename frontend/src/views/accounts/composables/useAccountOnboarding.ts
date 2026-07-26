import type { getAccounts } from '@/api'

import { computed, ref, shallowRef, watch } from 'vue'
import {
  completeAccountOAuth,
  importAccounts,
  startAccountOAuth,
} from '@/api'
import { ApiError } from '@/api/request'
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
  configRevision: { value: number }
}) {
  const createModalOpen = shallowRef(false)
  const reauthorizingAccount = shallowRef<AccountRow | null>(null)
  const configRevision = options.configRevision
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
        const result = await withConflictRefresh(() => startAccountOAuth({
          ...input,
          ...(account
            ? {
                accountId: account.id,
                expectedCredentialRevision: account.credentialRevision,
              }
            : {}),
        }))

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
        if (!callbackUrl)
          throw new Error('请粘贴 OAuth 回调地址')
        const result = await withConflictRefresh(() => completeAccountOAuth({
          provider: createForm.value.provider,
          flowId: createForm.value.oauthFlowId,
          callbackUrl,
        }))
        commitConfigRevision(result.configRevision)
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
    if (account.provider !== 'openai')
      return
    reauthorizingAccount.value = account
    createForm.value = {
      ...emptyCreateForm(),
      provider: 'openai',
      name: account.name,
      mode: 'oauth',
    }
    showCreateModal.value = true
    void handleAuthorizeOAuth()
  }

  async function requireConfigRevision() {
    if (configRevision.value <= 0)
      await options.reload()
    if (configRevision.value <= 0)
      throw new Error('当前配置 revision 不可用')
    return configRevision.value
  }

  function commitConfigRevision(revision: number) {
    if (revision > 0)
      configRevision.value = revision
  }

  async function newAccountInput() {
    const account = reauthorizingAccount.value
    return {
      provider: createForm.value.provider,
      expectedConfigRevision: await requireConfigRevision(),
      name: account?.name || account?.email || `${createForm.value.provider} OAuth`,
    }
  }

  async function importAccountDocument() {
    const data = parseImportJson(createForm.value.importText)
    if (Array.isArray(data) || typeof data !== 'object' || data === null)
      throw new Error('导入文件必须是 JSON object')
    const expectedConfigRevision = await requireConfigRevision()
    const result = await withConflictRefresh(() => importAccounts({
      provider: createForm.value.provider,
      expectedConfigRevision,
      data,
    }))
    commitConfigRevision(result.configRevision)
    return createForm.value.provider === 'xai' ? 'xAI 账号已导入' : 'OpenAI 账号已导入'
  }

  async function importMixedAccountDocument() {
    const documents = parseMixedImportDocuments(parseImportJson(createForm.value.importText))
    let importedCount = 0
    const failures: string[] = []

    for (const entry of documents) {
      try {
        const expectedConfigRevision = await requireConfigRevision()
        const result = await withConflictRefresh(() => importAccounts({
          provider: entry.provider,
          expectedConfigRevision,
          data: entry.document,
        }))
        commitConfigRevision(result.configRevision)
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
      throw new Error(failures.length > 0 ? `批量导入失败：${failures.join('；')}` : '批量文件没有可导入的账号')
    }

    if (failures.length > 0) {
      return `已导入 ${importedCount} 个账号，${failures.length} 个文档失败：${failures.join('；')}`
    }
    return `已导入 ${importedCount} 个账号`
  }

  async function finishCreate(message: string) {
    showCreateModal.value = false
    await options.reload()
    toast.success(message)
  }

  async function withConflictRefresh<T>(task: () => Promise<T>) {
    try {
      return await task()
    }
    catch (error) {
      if (error instanceof ApiError && error.status === 409) {
        await options.reload()
      }
      throw error
    }
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
    configRevision,
    createForm,
    handleCreate,
    handleAuthorizeOAuth,
    openCreateAccount,
    openReauthorizeAccount,
    requireConfigRevision,
    commitConfigRevision,
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
