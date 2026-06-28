import { clamp } from 'es-toolkit'
import { computed, onMounted, ref, type Ref } from 'vue'
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
import { useJsonDownload } from '@/composables/useJsonDownload'
import { withMinimumDuration } from '@/utils/async'
import { editableStatusOptions } from '../constants'

export function useAccountMutations(options: {
  page: Ref<number>
  pageSize: Ref<number>
  searchQuery: Ref<string>
  selectedIds: Ref<Set<string>>
  totalAccounts: Ref<number>
}) {
  const { downloadJson } = useJsonDownload()
  const loading = ref(true)
  const accounts = ref<any[]>([])
  const accountSummary = ref({
    total: 0,
    active: 0,
    highUsage: 0,
    attention: 0,
  })
  const showCreateModal = ref(false)
  const showDeleteModal = ref(false)
  const showSingleDeleteModal = ref(false)
  const editingAccount = ref<any>(null)
  const pendingDeleteAccount = ref<any>(null)
  const refreshingAccountIds = ref<Set<string>>(new Set())
  const refreshingQuotaAccountIds = ref<Set<string>>(new Set())
  const updatingStatusAccountIds = ref<Set<string>>(new Set())
  const deletingAccount = ref(false)
  const creatingAccount = ref(false)
  const authorizingOAuth = ref(false)
  const savingAccount = ref(false)
  const batchDeleting = ref(false)
  const exportingAccounts = ref(false)

  const createForm = ref({
    mode: 'oauth',
    refreshToken: '',
    importText: '',
    oauthSessionId: '',
    oauthAuthUrl: '',
    oauthCallback: '',
  })

  const editForm = ref({
    label: '',
    email: '',
    accountId: '',
    userId: '',
    planType: '',
    status: 'active',
  })

  const showEditModal = computed({
    get: () => editingAccount.value !== null,
    set: (value: boolean) => {
      if (!value) {
        editingAccount.value = null
      }
    },
  })

  const editStatusModel = computed<string>({
    get: () => editForm.value.status,
    set: (value) => {
      if (editableStatusOptions.some((option) => option.value === value)) {
        editForm.value.status = value
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

    try {
      const payload = accountImportPayload()
      if (!payload) return

      creatingAccount.value = true
      const result = await importAccounts(payload)
      showCreateModal.value = false
      resetCreateForm()
      await loadAccounts()
      toast.success(importSuccessText(result))
    } catch (error) {
      toast.error(error instanceof Error ? error.message : '导入失败')
    } finally {
      creatingAccount.value = false
    }
  }

  function accountImportPayload() {
    if (createForm.value.mode === 'oauth') {
      if (!createForm.value.oauthSessionId || !createForm.value.oauthCallback.trim()) return null
      return {
        sessionId: createForm.value.oauthSessionId,
        callbackUrl: createForm.value.oauthCallback.trim(),
      }
    }

    if (createForm.value.mode === 'rt') {
      const accounts = createForm.value.refreshToken
        .split(/\r?\n/)
        .map((token) => token.trim())
        .filter(Boolean)
        .map((refreshToken) => ({ refreshToken }))

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

    const sourceFormat = createForm.value.mode === 'sub2api' ? 'sub2api' : 'cpr'
    if (Array.isArray(parsed)) {
      return { sourceFormat, accounts: parsed }
    }
    if (parsed && typeof parsed === 'object' && Array.isArray(parsed.accounts)) {
      return { ...parsed, sourceFormat }
    }
    return { sourceFormat, accounts: [parsed] }
  }

  async function handleAuthorizeOAuth() {
    if (authorizingOAuth.value) return

    try {
      authorizingOAuth.value = true
      const result = await authorizeAccountOAuth()
      createForm.value = {
        ...createForm.value,
        mode: 'oauth',
        oauthSessionId: result.sessionId,
        oauthAuthUrl: result.authUrl,
        oauthCallback: '',
      }
      toast.success('授权链接已生成')
    } catch (error: any) {
      toast.error(error.message || '授权链接生成失败')
    } finally {
      authorizingOAuth.value = false
    }
  }

  async function handleExchangeOAuth() {
    if (creatingAccount.value) return

    try {
      const payload = accountImportPayload()
      if (!payload) return

      creatingAccount.value = true
      const result = await exchangeAccountOAuth(payload)
      showCreateModal.value = false
      resetCreateForm()
      await loadAccounts()
      toast.success(importSuccessText(result))
    } catch (error: any) {
      toast.error(error.message || 'OAuth 授权导入失败')
    } finally {
      creatingAccount.value = false
    }
  }

  function resetCreateForm() {
    createForm.value = {
      mode: 'oauth',
      refreshToken: '',
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

  function openEditAccount(account: any) {
    editingAccount.value = account
    editForm.value = {
      label: account.label || '',
      email: account.email || '',
      accountId: account.accountId || '',
      userId: account.userId || '',
      planType: account.planType || '',
      status: account.status,
    }
  }

  async function handleSaveAccount() {
    if (savingAccount.value) return

    const account = editingAccount.value
    if (!account) return

    try {
      savingAccount.value = true
      await updateAccount({
        id: account.id,
        label: nullableTrimmedValue(editForm.value.label),
        email: nullableTrimmedValue(editForm.value.email),
        accountId: nullableTrimmedValue(editForm.value.accountId),
        userId: nullableTrimmedValue(editForm.value.userId),
        planType: nullableTrimmedValue(editForm.value.planType),
        status: editForm.value.status,
      })
      editingAccount.value = null
      await loadAccounts()
      toast.success('账号已更新')
    } catch (error: any) {
      toast.error(error.message || '保存失败')
    } finally {
      savingAccount.value = false
    }
  }

  function nullableTrimmedValue(value: string) {
    const trimmed = value.trim()
    return trimmed || null
  }

  function requestDeleteAccount(account: any) {
    pendingDeleteAccount.value = account
    showSingleDeleteModal.value = true
  }

  async function handleDelete() {
    if (deletingAccount.value) return

    const accountId = pendingDeleteAccount.value?.id
    if (!accountId) return

    try {
      deletingAccount.value = true
      await deleteAccounts({ ids: [accountId] })
      showSingleDeleteModal.value = false
      pendingDeleteAccount.value = null
      await loadAccounts()
      toast.success('账号已删除')
    } catch (error: any) {
      toast.error(error.message || '删除失败')
    } finally {
      deletingAccount.value = false
    }
  }

  async function handleBatchDelete() {
    if (batchDeleting.value) return
    if (options.selectedIds.value.size === 0) return

    try {
      batchDeleting.value = true
      await deleteAccounts({ ids: [...options.selectedIds.value] })
      options.selectedIds.value = new Set()
      showDeleteModal.value = false
      await loadAccounts()
    } catch (error) {
      console.error('Failed to batch delete accounts:', error)
    } finally {
      batchDeleting.value = false
    }
  }

  async function handleExportAccounts() {
    if (exportingAccounts.value) return

    try {
      exportingAccounts.value = true
      const selected = [...options.selectedIds.value]
      const payload = await exportAccounts(
        selected.length ? { ids: selected.join(',') } : undefined,
      )
      await downloadJson(payload, exportFileName(selected.length))
      toast.success(selected.length ? `已导出 ${selected.length} 个账号` : '账号已导出')
    } catch (error: any) {
      toast.error(error.message || '导出失败')
    } finally {
      exportingAccounts.value = false
    }
  }

  async function handleRefresh(accountId: string) {
    if (refreshingAccountIds.value.has(accountId)) return
    refreshingAccountIds.value = new Set(refreshingAccountIds.value).add(accountId)
    try {
      await withMinimumDuration(async () => {
        await refreshAccount({ id: accountId })
        await loadAccounts()
      })
      toast.success('Token 已刷新')
    } catch (error: any) {
      toast.error(error.message || '刷新失败')
    } finally {
      const next = new Set(refreshingAccountIds.value)
      next.delete(accountId)
      refreshingAccountIds.value = next
    }
  }

  async function handleRefreshQuota(accountId: string) {
    if (refreshingQuotaAccountIds.value.has(accountId)) return
    refreshingQuotaAccountIds.value = new Set(refreshingQuotaAccountIds.value).add(accountId)
    try {
      await withMinimumDuration(async () => {
        await getAccountQuota({ id: accountId })
        await loadAccounts()
      })
    } catch (error) {
      console.error('Failed to refresh account quota:', error)
    } finally {
      const next = new Set(refreshingQuotaAccountIds.value)
      next.delete(accountId)
      refreshingQuotaAccountIds.value = next
    }
  }

  async function handleToggleSchedule(account: any) {
    if (updatingStatusAccountIds.value.has(account.id)) return
    const nextStatus = account.status === 'disabled' ? 'active' : 'disabled'
    updatingStatusAccountIds.value = new Set(updatingStatusAccountIds.value).add(account.id)
    try {
      await updateAccount({ id: account.id, status: nextStatus })
      await loadAccounts()
      toast.success(nextStatus === 'disabled' ? '已禁用调度' : '已启用调度')
    } catch (error: any) {
      toast.error(error.message || '状态更新失败')
    } finally {
      const next = new Set(updatingStatusAccountIds.value)
      next.delete(account.id)
      updatingStatusAccountIds.value = next
    }
  }

  function scheduleActionLabel(account: any) {
    return account.status === 'disabled' ? '启用调度' : '禁用调度'
  }

  function exportFileName(selectedCount: number) {
    const suffix = selectedCount > 0 ? `selected-${selectedCount}` : 'all'
    return `cpr-accounts-${suffix}-${dayjs().format('YYYY-MM-DD')}.json`
  }

  onMounted(() => {
    loadAccounts()
  })

  return {
    loading,
    accounts,
    accountSummary,
    showCreateModal,
    showDeleteModal,
    showSingleDeleteModal,
    showEditModal,
    editingAccount,
    pendingDeleteAccount,
    refreshingAccountIds,
    refreshingQuotaAccountIds,
    updatingStatusAccountIds,
    deletingAccount,
    creatingAccount,
    authorizingOAuth,
    savingAccount,
    batchDeleting,
    exportingAccounts,
    createForm,
    editForm,
    editStatusModel,
    loadAccounts,
    handleCreate,
    handleAuthorizeOAuth,
    handleExchangeOAuth,
    openEditAccount,
    handleSaveAccount,
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
