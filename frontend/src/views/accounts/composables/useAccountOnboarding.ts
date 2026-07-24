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

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]

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
        await importAccountDocument()
        await finishCreate(
          createForm.value.provider === 'xai' ? 'xAI OAuth 账号已导入' : 'OpenAI 账号已导入',
        )
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
        mode: 'oauth',
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
