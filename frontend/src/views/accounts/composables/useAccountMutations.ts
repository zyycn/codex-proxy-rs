import { clamp } from 'es-toolkit'
import { computed, onMounted, onUnmounted, ref, watch, type Ref } from 'vue'
import dayjs from 'dayjs'

import {
  authorizeAccountOAuth,
  deleteAccounts,
  exchangeAccountOAuth,
  exportAccounts,
  getAccountQuota,
  getAccounts,
  importAccounts,
  refreshAccount,
  updateAccount,
} from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useDownload } from '@/composables/useDownload'
import { useIdSet } from '@/composables/useIdSet'
import { withMinimumDuration } from '@/utils/async'

type TokenImportAccount = { token: string } | { refreshToken: string }
type AccountRow = {
  id: string
  status: string
  displayStatus: string
  tokenRefreshing: boolean
  planType?: string | null
  quota: any
  usage: any
  [key: string]: any
}

type AccountQuotaRefreshResult = {
  account: AccountRow
}

type AccountRefreshResult = {
  result: 'alive' | 'dead' | 'skipped'
  error?: string | null
}

export function useAccountMutations(options: {
  page: Ref<number>
  pageSize: Ref<number>
  searchQuery: Ref<string>
  selectedIds: Ref<Set<string>>
  totalAccounts: Ref<number>
}) {
  const { downloadJson } = useDownload()
  const loading = ref(true)
  const accounts = ref<AccountRow[]>([])
  const accountSummary = ref({
    total: 0,
    active: 0,
    highUsage: 0,
    attention: 0,
  })
  const createModalOpen = ref(false)
  const showDeleteModal = ref(false)
  const showSingleDeleteModal = ref(false)
  const reauthorizingAccount = ref<any>(null)
  const pendingDeleteAccount = ref<any>(null)
  const refreshingAccounts = useIdSet<string>()
  const refreshingQuotaAccounts = useIdSet<string>()
  const updatingStatusAccounts = useIdSet<string>()
  const deletingAccountAction = useAsyncAction()
  const creatingAccountAction = useAsyncAction()
  const authorizingOAuthAction = useAsyncAction()
  const batchDeletingAction = useAsyncAction()
  const exportingAccountsAction = useAsyncAction()
  const refreshingAccountIds = refreshingAccounts.ids
  const refreshingQuotaAccountIds = refreshingQuotaAccounts.ids
  const updatingStatusAccountIds = updatingStatusAccounts.ids
  const deletingAccount = deletingAccountAction.loading
  const creatingAccount = creatingAccountAction.loading
  const authorizingOAuth = authorizingOAuthAction.loading
  const batchDeleting = batchDeletingAction.loading
  const exportingAccounts = exportingAccountsAction.loading
  let tokenRefreshPollTimer: ReturnType<typeof window.setInterval> | undefined

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
      if (!value) {
        reauthorizingAccount.value = null
      }
    },
  })

  async function loadAccounts() {
    try {
      loading.value = true
      const result = await getAccounts({
        page: options.page.value,
        pageSize: options.pageSize.value,
        search: options.searchQuery.value,
      })
      accounts.value = result.items
      accountSummary.value = result.summary
      options.totalAccounts.value = result.page.total ?? result.items.length
      options.page.value = result.page.page ?? options.page.value
      options.pageSize.value = result.page.pageSize ?? options.pageSize.value

      if (
        accounts.value.length === 0 &&
        options.totalAccounts.value > 0 &&
        options.page.value > 1
      ) {
        options.page.value = clamp(
          result.page.totalPages ?? options.page.value - 1,
          1,
          Number.POSITIVE_INFINITY,
        )
        await loadAccounts()
      }
    } finally {
      loading.value = false
    }
  }

  async function handleCreate() {
    if (createForm.value.mode === 'oauth') {
      await handleExchangeOAuth()
      return
    }
    if (creatingAccount.value) return

    await creatingAccountAction.run(
      async () => {
        const payload = accountImportPayload()
        if (!payload) return

        const result = await importAccounts(payload)
        showCreateModal.value = false
        resetCreateForm()
        await loadAccounts()
        toast.success(importSuccessText(result))
      },
      { errorText: '导入失败' },
    )
  }

  function accountImportPayload() {
    if (createForm.value.mode === 'oauth') {
      if (!createForm.value.oauthSessionId || !createForm.value.oauthCallback.trim()) return null
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
    if (!text) return null

    let parsed
    try {
      parsed = JSON.parse(text)
    } catch {
      throw new Error('JSON 格式不正确')
    }

    const sourceFormat = accountImportSourceFormat()
    if (Array.isArray(parsed)) {
      return { sourceFormat, accounts: parsed }
    }
    if (parsed && typeof parsed === 'object' && Array.isArray(parsed.accounts)) {
      return { ...parsed, sourceFormat }
    }
    return { sourceFormat, accounts: [parsed] }
  }

  function accountImportSourceFormat() {
    if (createForm.value.mode === 'sub2api') return 'sub2api'
    if (createForm.value.mode === 'cliproxyapi') return 'cliproxyapi'
    return 'cpr'
  }

  function accountFromTokenLine(line: string): TokenImportAccount | null {
    const token = line.trim()
    if (!token) return null
    if (token.startsWith('rt_')) {
      return { refreshToken: token }
    }
    return { token }
  }

  async function handleAuthorizeOAuth() {
    if (authorizingOAuth.value) return

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

  async function handleExchangeOAuth() {
    if (creatingAccount.value) return

    await creatingAccountAction.run(
      async () => {
        const payload = accountImportPayload()
        if (!payload) return

        const result = await exchangeAccountOAuth(payload)
        const successText = oauthSuccessText(result)
        showCreateModal.value = false
        resetCreateForm()
        await loadAccounts()
        toast.success(successText)
      },
      { errorText: reauthorizingAccount.value ? '重新授权失败' : 'OAuth 授权导入失败' },
    )
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

  function importSuccessText(result: any) {
    const imported = result?.imported ?? 0
    const skipped = result?.skipped ?? 0
    if (skipped > 0) {
      return `导入完成，写入 ${imported} 个，跳过 ${skipped} 个`
    }
    return `导入完成，写入 ${imported} 个`
  }

  function oauthSuccessText(result: any) {
    if (reauthorizingAccount.value) {
      return '账号重新授权成功'
    }
    return importSuccessText(result)
  }

  function openCreateAccount() {
    reauthorizingAccount.value = null
    resetCreateForm()
    showCreateModal.value = true
  }

  function openReauthorizeAccount(account: any) {
    reauthorizingAccount.value = account
    resetCreateForm()
    showCreateModal.value = true
    void handleAuthorizeOAuth()
  }

  function requestDeleteAccount(account: any) {
    pendingDeleteAccount.value = account
    showSingleDeleteModal.value = true
  }

  async function handleDelete() {
    if (deletingAccount.value) return

    const accountId = pendingDeleteAccount.value?.id
    if (!accountId) return

    await deletingAccountAction.run(
      async () => {
        await deleteAccounts({ ids: [accountId] })
        showSingleDeleteModal.value = false
        pendingDeleteAccount.value = null
        await loadAccounts()
        toast.success('账号已删除')
      },
      { errorText: '删除失败' },
    )
  }

  async function handleBatchDelete() {
    if (batchDeleting.value) return
    if (options.selectedIds.value.size === 0) return

    await batchDeletingAction.run(
      async () => {
        await deleteAccounts({ ids: [...options.selectedIds.value] })
        options.selectedIds.value = new Set()
        showDeleteModal.value = false
        await loadAccounts()
      },
      {
        errorText: false,
        onError: (error) => {
          console.error('Failed to batch delete accounts:', error)
        },
      },
    )
  }

  async function handleExportAccounts() {
    if (exportingAccounts.value) return
    const selected = [...options.selectedIds.value]
    if (selected.length === 0) {
      toast.warning('请选择要导出的账号')
      return
    }

    await exportingAccountsAction.run(
      async () => {
        const payload = await exportAccounts({ ids: selected.join(',') })
        await downloadJson(payload, exportFileName(selected.length))
        toast.success(`已导出 ${selected.length} 个账号`)
      },
      { errorText: '导出失败' },
    )
  }

  async function handleRefresh(accountId: string) {
    await refreshingAccounts.run(accountId, async () => {
      try {
        const result = await withMinimumDuration(async () => {
          const result = (await refreshAccount({ id: accountId })) as AccountRefreshResult
          await loadAccounts()
          return result
        })
        if (result.result === 'alive') {
          toast.success('Token 已刷新')
        } else if (result.result === 'skipped') {
          toast.warning(result.error || 'Token 正在刷新中')
        } else {
          toast.error(result.error || '刷新失败')
        }
      } catch (error: any) {
        toast.error(error.message || '刷新失败')
      }
    })
  }

  async function handleRefreshQuota(accountId: string) {
    await refreshingQuotaAccounts.run(accountId, async () => {
      try {
        await withMinimumDuration(async () => {
          const result = (await getAccountQuota({ id: accountId })) as AccountQuotaRefreshResult
          mergeAccountQuotaRefresh(accountId, result)
        })
      } catch (error) {
        console.error('Failed to refresh account quota:', error)
      }
    })
  }

  function mergeAccountQuotaRefresh(accountId: string, result: AccountQuotaRefreshResult) {
    accounts.value = accounts.value.map((account) =>
      account.id === accountId ? { ...account, ...result.account } : account,
    )
  }

  async function handleToggleSchedule(account: any) {
    const nextStatus = account.status === 'disabled' ? 'active' : 'disabled'
    await updatingStatusAccounts.run(account.id, async () => {
      try {
        await updateAccount({ id: account.id, status: nextStatus })
        await loadAccounts()
        toast.success(nextStatus === 'disabled' ? '已禁用调度' : '已启用调度')
      } catch (error: any) {
        toast.error(error.message || '状态更新失败')
      }
    })
  }

  function scheduleActionLabel(account: any) {
    return account.status === 'disabled' ? '启用调度' : '禁用调度'
  }

  function exportFileName(selectedCount: number) {
    return `cpr-accounts-selected-${selectedCount}-${dayjs().format('YYYY-MM-DD')}.json`
  }

  function hasTokenRefreshingAccount() {
    return accounts.value.some((account) => account.tokenRefreshing)
  }

  function startTokenRefreshPolling() {
    if (tokenRefreshPollTimer !== undefined) return
    tokenRefreshPollTimer = window.setInterval(() => {
      if (!loading.value && hasTokenRefreshingAccount()) {
        void loadAccounts()
      }
    }, 2000)
  }

  function stopTokenRefreshPolling() {
    if (tokenRefreshPollTimer === undefined) return
    window.clearInterval(tokenRefreshPollTimer)
    tokenRefreshPollTimer = undefined
  }

  watch(
    () => hasTokenRefreshingAccount(),
    (hasRefreshing) => {
      if (hasRefreshing) {
        startTokenRefreshPolling()
      } else {
        stopTokenRefreshPolling()
      }
    },
  )

  onMounted(() => {
    loadAccounts()
  })

  onUnmounted(() => {
    stopTokenRefreshPolling()
  })

  return {
    loading,
    accounts,
    accountSummary,
    showCreateModal,
    showDeleteModal,
    showSingleDeleteModal,
    reauthorizingAccount,
    pendingDeleteAccount,
    refreshingAccountIds,
    refreshingQuotaAccountIds,
    updatingStatusAccountIds,
    deletingAccount,
    creatingAccount,
    authorizingOAuth,
    batchDeleting,
    exportingAccounts,
    createForm,
    loadAccounts,
    handleCreate,
    handleAuthorizeOAuth,
    handleExchangeOAuth,
    openCreateAccount,
    openReauthorizeAccount,
    requestDeleteAccount,
    handleDelete,
    handleBatchDelete,
    handleExportAccounts,
    handleRefresh,
    handleRefreshQuota,
    handleToggleSchedule,
    scheduleActionLabel,
  }
}
