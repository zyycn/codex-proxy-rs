import type {
  getAccounts,
} from '@/api'

import { computed, ref } from 'vue'
import {
  authorizeAccountOAuth,
  exchangeAccountOAuth,
  importAccounts,
} from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]
type TokenImportAccount = { token: string } | { refreshToken: string }

export function useAccountOnboarding(options: { reload: () => Promise<unknown> }) {
  const createModalOpen = ref(false)
  const reauthorizingAccount = ref<AccountRow | null>(null)
  const creatingAccountAction = useAsyncAction()
  const authorizingOAuthAction = useAsyncAction()
  const creatingAccount = creatingAccountAction.loading
  const authorizingOAuth = authorizingOAuthAction.loading
  const createForm = ref({
    mode: 'oauth',
    tokenText: '',
    importText: '',
    oauthSessionId: '',
    oauthAuthUrl: '',
    oauthCallback: '',
  })

  const showCreateModal = computed({
    get: () => createModalOpen.value,
    set: (value: boolean) => {
      createModalOpen.value = value
      if (!value)
        reauthorizingAccount.value = null
    },
  })

  async function handleCreate() {
    if (createForm.value.mode === 'oauth') {
      await exchangeOAuth()
      return
    }
    if (creatingAccount.value)
      return

    await creatingAccountAction.run(
      async () => {
        const payload = accountImportPayload()
        if (!payload)
          return

        const result = await importAccounts(payload)
        showCreateModal.value = false
        resetCreateForm()
        await options.reload()
        toast.success(importSuccessText(result))
      },
      { errorText: '导入失败' },
    )
  }

  async function handleAuthorizeOAuth() {
    if (authorizingOAuth.value)
      return

    await authorizingOAuthAction.run(
      async () => {
        const result = await authorizeAccountOAuth()
        createForm.value = {
          ...createForm.value,
          mode: 'oauth',
          oauthSessionId: result.sessionId,
          oauthAuthUrl: result.authUrl,
          oauthCallback: '',
        }
        toast.success('授权链接已生成')
      },
      { errorText: '授权链接生成失败' },
    )
  }

  async function exchangeOAuth() {
    if (creatingAccount.value)
      return

    await creatingAccountAction.run(
      async () => {
        const payload = accountImportPayload()
        if (!payload)
          return

        const result = await exchangeAccountOAuth(payload)
        const successText = reauthorizingAccount.value
          ? '账号重新授权成功'
          : importSuccessText(result)
        showCreateModal.value = false
        resetCreateForm()
        await options.reload()
        toast.success(successText)
      },
      { errorText: reauthorizingAccount.value ? '重新授权失败' : 'OAuth 授权导入失败' },
    )
  }

  function openCreateAccount() {
    reauthorizingAccount.value = null
    resetCreateForm()
    showCreateModal.value = true
  }

  function openReauthorizeAccount(account: AccountRow) {
    reauthorizingAccount.value = account
    resetCreateForm()
    showCreateModal.value = true
    void handleAuthorizeOAuth()
  }

  function accountImportPayload() {
    if (createForm.value.mode === 'oauth') {
      if (!createForm.value.oauthSessionId || !createForm.value.oauthCallback.trim())
        return null
      return {
        sessionId: createForm.value.oauthSessionId,
        callbackUrl: createForm.value.oauthCallback.trim(),
      }
    }

    if (createForm.value.mode === 'token') {
      const accounts = createForm.value.tokenText
        .split(/\r?\n/)
        .map(accountFromTokenLine)
        .filter((account): account is TokenImportAccount => account !== null)
      return accounts.length ? { sourceFormat: 'cpr', accounts } : null
    }

    const text = createForm.value.importText.trim()
    if (!text)
      return null

    let parsed: unknown
    try {
      parsed = JSON.parse(text)
    }
    catch {
      throw new Error('JSON 格式不正确')
    }

    const sourceFormat = accountImportSourceFormat()
    if (Array.isArray(parsed))
      return { sourceFormat, accounts: parsed }
    if (isObjectWithAccounts(parsed))
      return { ...parsed, sourceFormat }
    return { sourceFormat, accounts: [parsed] }
  }

  function accountImportSourceFormat() {
    if (createForm.value.mode === 'sub2api')
      return 'sub2api'
    if (createForm.value.mode === 'cliproxyapi')
      return 'cliproxyapi'
    return 'cpr'
  }

  function resetCreateForm() {
    createForm.value = {
      mode: 'oauth',
      tokenText: '',
      importText: '',
      oauthSessionId: '',
      oauthAuthUrl: '',
      oauthCallback: '',
    }
  }

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

function accountFromTokenLine(line: string): TokenImportAccount | null {
  const token = line.trim()
  if (!token)
    return null
  return token.startsWith('rt_') ? { refreshToken: token } : { token }
}

function importSuccessText(result: Awaited<ReturnType<typeof importAccounts>>) {
  if (result.skipped > 0) {
    return `导入完成，写入 ${result.imported} 个，跳过 ${result.skipped} 个`
  }
  return `导入完成，写入 ${result.imported} 个`
}

function isObjectWithAccounts(value: unknown): value is Record<string, unknown> & {
  accounts: unknown[]
} {
  return (
    typeof value === 'object'
    && value !== null
    && 'accounts' in value
    && Array.isArray(value.accounts)
  )
}
