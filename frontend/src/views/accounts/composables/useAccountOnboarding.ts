import type { getAccounts, getProviderInstances } from '@/api'

import { computed, ref, shallowRef, watch } from 'vue'
import {
  completeCodexOAuthAuthorization,
  completeXaiOAuthAuthorization,
  getProviderInstances as fetchProviderInstances,
  importCodexCredentialsDocument,
  importXaiCredentialsDocument,
  startCodexOAuthAuthorization,
  startXaiOAuthAuthorization,
} from '@/api'
import { ApiError } from '@/api/request'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]
type ProviderInstance = Awaited<ReturnType<typeof getProviderInstances>>['items'][number]

export function useAccountOnboarding(options: {
  reload: () => Promise<unknown>
  configRevision: { value: number }
}) {
  const createModalOpen = shallowRef(false)
  const reauthorizingAccount = shallowRef<AccountRow | null>(null)
  const providerInstances = shallowRef<ProviderInstance[]>([])
  const configRevision = options.configRevision
  const loadingProviderInstances = shallowRef(false)
  const creatingAccountAction = useAsyncAction()
  const authorizingOAuthAction = useAsyncAction()
  const creatingAccount = creatingAccountAction.loading
  const authorizingOAuth = authorizingOAuthAction.loading
  const createForm = ref(emptyCreateForm())

  let providerInstancesPromise: Promise<void> | undefined

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

  const providerInstanceOptions = computed(() =>
    providerInstances.value
      .filter(instance =>
        instance.enabled && instance.providerKind === createForm.value.provider,
      )
      .map(instance => ({
        label: instance.name,
        value: instance.id,
      })),
  )

  async function handleCreate() {
    if (createForm.value.mode === 'oauth') {
      await completeOAuth()
      return
    }
    if (creatingAccount.value)
      return

    await creatingAccountAction.run(
      async () => {
        if (createForm.value.provider === 'xai') {
          await importXaiAccountDocument()
          await finishCreate('xAI OAuth 账号已导入')
          return
        }
        await importCodexDocument()
        await finishCreate('OpenAI 账号已导入')
      },
      { errorText: '导入失败' },
    )
  }

  async function handleAuthorizeOAuth() {
    if (authorizingOAuth.value)
      return

    await authorizingOAuthAction.run(
      async () => {
        const input = await newCredentialInput()
        const account = reauthorizingAccount.value
        const result = await withConflictRefresh(() =>
          createForm.value.provider === 'xai'
            ? startXaiOAuthAuthorization(input)
            : startCodexOAuthAuthorization({
                ...input,
                ...(account
                  ? {
                      credentialId: account.id,
                      expectedCredentialRevision: account.credentialRevision,
                    }
                  : {}),
              }),
        )

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
        const result = await withConflictRefresh(() =>
          createForm.value.provider === 'xai'
            ? completeXaiOAuthAuthorization({
                flowId: createForm.value.oauthFlowId,
                callbackUrl,
              })
            : completeCodexOAuthAuthorization({
                flowId: createForm.value.oauthFlowId,
                callbackUrl,
              }),
        )
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
    void loadProviderInstances().catch(() => {
      toast.error('Provider instance 加载失败')
    })
  }

  function openReauthorizeAccount(account: AccountRow) {
    if (account.provider !== 'openai')
      return
    reauthorizingAccount.value = account
    createForm.value = {
      ...emptyCreateForm(),
      provider: 'openai',
      providerInstanceId: account.providerInstanceId,
      name: account.name,
      mode: 'oauth',
    }
    showCreateModal.value = true
    void loadProviderInstances()
      .then(() => handleAuthorizeOAuth())
      .catch(() => {
        toast.error('Provider instance 加载失败')
      })
  }

  async function loadProviderInstances() {
    if (providerInstancesPromise)
      return providerInstancesPromise

    providerInstancesPromise = (async () => {
      loadingProviderInstances.value = true
      try {
        for (let attempt = 0; attempt < 2; attempt += 1) {
          const items: ProviderInstance[] = []
          const seenCursors = new Set<string>()
          let cursor: string | undefined
          let revision = 0
          let changedDuringRead = false

          do {
            const result = await fetchProviderInstances({ cursor, limit: 200 })
            if (revision > 0 && result.configRevision !== revision) {
              changedDuringRead = true
              break
            }
            revision = result.configRevision
            items.push(...result.items)
            cursor = result.nextCursor || undefined
            if (cursor && seenCursors.has(cursor))
              throw new Error('Provider instance 分页游标重复')
            if (cursor)
              seenCursors.add(cursor)
          } while (cursor)

          if (changedDuringRead)
            continue

          providerInstances.value = items.filter(instance =>
            instance.providerKind === 'openai' || instance.providerKind === 'xai',
          )
          configRevision.value = revision
          selectDefaultProviderInstance()
          return
        }
        throw new Error('Provider instance 配置持续变化，请重试')
      }
      finally {
        loadingProviderInstances.value = false
      }
    })()

    try {
      await providerInstancesPromise
    }
    finally {
      providerInstancesPromise = undefined
    }
  }

  async function requireConfigRevision() {
    if (configRevision.value <= 0)
      await loadProviderInstances()
    if (configRevision.value <= 0)
      throw new Error('当前配置 revision 不可用')
    return configRevision.value
  }

  function commitConfigRevision(revision: number) {
    if (revision > 0)
      configRevision.value = revision
  }

  function selectDefaultProviderInstance() {
    if (
      providerInstanceOptions.value.some(
        option => option.value === createForm.value.providerInstanceId,
      )
    ) {
      return
    }
    createForm.value = {
      ...createForm.value,
      providerInstanceId: providerInstanceOptions.value[0]?.value || '',
    }
  }

  async function newCredentialInput() {
    if (!createForm.value.providerInstanceId)
      throw new Error('请选择 Provider instance')
    const account = reauthorizingAccount.value
    return {
      expectedConfigRevision: await requireConfigRevision(),
      providerInstanceId: createForm.value.providerInstanceId,
      name: account?.name || account?.email || `${createForm.value.provider} OAuth`,
    }
  }

  async function importCodexDocument() {
    const tokenMode = createForm.value.mode === 'token'
    const document = tokenMode
      ? {
          sourceFormat: 'cpr',
          accounts: createForm.value.tokenText
            .split(/\r?\n/)
            .map(token => token.trim())
            .filter(Boolean)
            .map(token => token.startsWith('rt_') ? { refreshToken: token } : { token }),
        }
      : parseImportJson(createForm.value.importText)
    const expectedConfigRevision = await requireConfigRevision()
    const result = await withConflictRefresh(() =>
      importCodexCredentialsDocument({
        expectedConfigRevision,
        providerInstanceId: createForm.value.providerInstanceId,
        document,
      }),
    )
    commitConfigRevision(result.configRevision)
  }

  async function importXaiAccountDocument() {
    const document = parseImportJson(createForm.value.importText)
    if (Array.isArray(document) || typeof document !== 'object' || document === null)
      throw new Error('xAI 导入文件必须是 JSON object')
    const expectedConfigRevision = await requireConfigRevision()
    const result = await withConflictRefresh(() =>
      importXaiCredentialsDocument({
        expectedConfigRevision,
        providerInstanceId: createForm.value.providerInstanceId,
        document,
      }),
    )
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
        await Promise.allSettled([
          loadProviderInstances(),
          options.reload(),
        ])
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
      selectDefaultProviderInstance()
    },
  )

  return {
    showCreateModal,
    reauthorizingAccount,
    creatingAccount,
    authorizingOAuth,
    loadingProviderInstances,
    providerInstanceOptions,
    configRevision,
    createForm,
    handleCreate,
    handleAuthorizeOAuth,
    openCreateAccount,
    openReauthorizeAccount,
    requireConfigRevision,
    commitConfigRevision,
    loadProviderInstances,
  }
}

function emptyCreateForm() {
  return {
    provider: 'openai',
    providerInstanceId: '',
    name: '',
    mode: 'oauth',
    tokenText: '',
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
      { label: 'JSON', value: 'json' },
    ]
  }
  return [
    { label: 'OAuth', value: 'oauth' },
    { label: 'Token', value: 'token' },
    { label: 'JSON', value: 'json' },
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
