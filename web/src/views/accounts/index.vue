<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import {
  AlertTriangle,
  ChevronDown,
  CheckCircle2,
  Clock3,
  Gauge,
  MoreHorizontal,
  Pencil,
  Plus,
  Power,
  RefreshCw,
  Search,
  ShieldCheck,
  Trash2,
  Users,
  Wifi,
  XCircle,
} from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BasePopover from '@/components/base/BasePopover.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseTable from '@/components/base/BaseTable.vue'
import { toast } from '@/components/base/BaseToast'
import { withMinimumDuration } from '@/utils/async'

import {
  createAccount,
  deleteAccounts,
  getAccountTestModels,
  getAccountQuota,
  getAccounts,
  refreshAccount,
  testAccountConnectionStream,
  updateAccount,
} from '@/api'

const loading = ref(true)
const accounts = ref<any[]>([])
const totalAccounts = ref(0)
const accountSummary = ref({
  total: 0,
  active: 0,
  highUsage: 0,
  attention: 0,
})
const page = ref(1)
const pageSize = ref(20)
const searchQuery = ref('')
const selectedIds = ref<Set<string>>(new Set())
const expandedAccountIds = ref<Set<string>>(new Set())
const showCreateModal = ref(false)
const showDeleteModal = ref(false)
const showSingleDeleteModal = ref(false)
const showConnectionTestModal = ref(false)
const editingAccount = ref<any>(null)
const pendingDeleteAccount = ref<any>(null)
const testingAccount = ref<any>(null)
const connectionTestStatus = ref('idle')
const connectionTestModel = ref('')
const connectionTestContent = ref('')
const connectionTestLogs = ref<any[]>([])
const connectionTestError = ref('')
const connectionTestStartedAt = ref('')
const connectionTestFinishedAt = ref('')
const connectionTestDurationMs = ref<number | null>(null)
const refreshingAccountIds = ref<Set<string>>(new Set())
const refreshingQuotaAccountIds = ref<Set<string>>(new Set())
const updatingStatusAccountIds = ref<Set<string>>(new Set())
const testingConnectionIds = ref<Set<string>>(new Set())
const deletingAccount = ref(false)
const loaded = ref(false)
const savingAccount = ref(false)
const batchDeleting = ref(false)
const loadingConnectionTestModels = ref(false)
const connectionTestSelectedModel = ref('')
const connectionTestModelOptions = ref<any[]>([])
let searchTimer: number | undefined
let connectionTestAbortController: AbortController | undefined
let connectionTestStartedAtMs = 0

const createForm = ref({
  refreshToken: '',
})

const editForm = ref<{
  label: string
  email: string
  accountId: string
  userId: string
  planType: string
  status: string
}>({
  label: '',
  email: '',
  accountId: '',
  userId: '',
  planType: '',
  status: 'active',
})

const relaxedCellClass = 'py-3 align-middle'

const accountColumns = [
  {
    key: 'expander',
    label: '',
    width: '40px',
    align: 'center' as const,
    headerClass: '!px-2',
    cellClass: `!px-2 ${relaxedCellClass}`,
  },
  {
    key: 'selection',
    label: '',
    width: '40px',
    align: 'center' as const,
    headerClass: '!px-2',
    cellClass: `!px-2 ${relaxedCellClass}`,
  },
  { key: 'identity', label: '邮箱', flex: 2.6, minWidth: '260px', cellClass: relaxedCellClass },
  { key: 'status', label: '状态', flex: 0.8, cellClass: relaxedCellClass },
  { key: 'planType', label: '套餐', flex: 0.8, cellClass: relaxedCellClass },
  { key: 'usage', label: '用量', flex: 1.5, minWidth: '220px', cellClass: relaxedCellClass },
  { key: 'updatedAt', label: '最后使用', flex: 1.2, cellClass: relaxedCellClass },
  {
    key: 'accessTokenExpiresAt',
    label: '过期时间',
    flex: 1.2,
    cellClass: relaxedCellClass,
  },
  {
    key: 'actions',
    label: '操作',
    width: '116px',
    align: 'right' as const,
    headerClass: '!pr-3',
    cellClass: `!px-2 ${relaxedCellClass}`,
  },
]

const statusLabels: Record<string, string> = {
  active: '正常',
  expired: '已过期',
  disabled: '已禁用',
  banned: '已封禁',
  quota_exhausted: '配额耗尽',
  refreshing: '刷新中',
}

const editableStatusOptions: Array<{ value: string; label: string }> = [
  { value: 'active', label: statusLabels.active },
  { value: 'disabled', label: statusLabels.disabled },
  { value: 'expired', label: statusLabels.expired },
  { value: 'quota_exhausted', label: statusLabels.quota_exhausted },
  { value: 'refreshing', label: statusLabels.refreshing },
  { value: 'banned', label: statusLabels.banned },
]

const editStatusModel = computed<string>({
  get: () => editForm.value.status,
  set: (value) => {
    if (editableStatusOptions.some((option) => option.value === value)) {
      editForm.value.status = value
    }
  },
})

const statusTones: Record<string, 'success' | 'danger' | 'warning' | 'info' | 'normal'> = {
  active: 'success',
  expired: 'warning',
  disabled: 'normal',
  banned: 'danger',
  quota_exhausted: 'warning',
  refreshing: 'info',
}

const filteredAccounts = computed(() => accounts.value)
const initialLoading = computed(() => loading.value && !loaded.value)

const allSelected = computed(
  () =>
    accounts.value.length > 0 &&
    accounts.value.every((account) => selectedIds.value.has(account.id)),
)

const indeterminate = computed(
  () => accounts.value.some((account) => selectedIds.value.has(account.id)) && !allSelected.value,
)

const selectedRowKeys = computed(() => [...selectedIds.value])
const accountPagination = computed(() => ({
  page: page.value,
  pageSize: pageSize.value,
  total: totalAccounts.value,
  pageSizes: [10, 20, 50, 100],
}))
const connectionTestStatusView = computed(() => {
  if (connectionTestStatus.value === 'running') {
    return {
      label: '正在测试',
      description: '正在发送一条真实 Responses 流式请求。',
      icon: Clock3,
      badge: 'bg-(--cp-info-bg) text-(--cp-info-text)',
      iconClass: 'text-(--cp-info)',
    }
  }
  if (connectionTestStatus.value === 'success') {
    return {
      label: '连接正常',
      description: '账号令牌可用，已完成 Codex Responses 流式验证。',
      icon: CheckCircle2,
      badge: 'bg-(--cp-success-bg) text-(--cp-success-text)',
      iconClass: 'text-(--cp-success)',
    }
  }
  if (connectionTestStatus.value === 'error') {
    return {
      label: '测试失败',
      description: '真实请求未完成，优先检查令牌状态、账号权限或上游网络。',
      icon: XCircle,
      badge: 'bg-(--cp-danger-bg) text-(--cp-danger-text)',
      iconClass: 'text-(--cp-danger)',
    }
  }
  return {
    label: '准备测试',
    description: '点击开始后发送一条轻量 Responses 流式请求。',
    icon: Wifi,
    badge: 'bg-(--cp-bg-subtle) text-(--cp-text-secondary)',
    iconClass: 'text-(--cp-text-muted)',
  }
})
const showEditModal = computed({
  get: () => editingAccount.value !== null,
  set: (value: boolean) => {
    if (!value) {
      editingAccount.value = null
    }
  },
})
const overviewItems = computed(() => [
  {
    label: '总账号',
    value: formatCount(accountSummary.value.total),
    caption: searchQuery.value.trim() ? '匹配筛选结果' : '账号池规模',
    tone: 'info',
    icon: Users,
  },
  {
    label: '正常账号',
    value: formatCount(accountSummary.value.active),
    caption: '可参与调度',
    tone: 'success',
    icon: ShieldCheck,
  },
  {
    label: '额度预警',
    value: formatCount(accountSummary.value.highUsage),
    caption: '任一窗口 >= 80%',
    tone: 'warning',
    icon: Gauge,
  },
  {
    label: '需处理',
    value: formatCount(accountSummary.value.attention),
    caption: '过期 / 禁用 / 封禁',
    tone: 'danger',
    icon: AlertTriangle,
  },
])

async function loadAccounts() {
  try {
    loading.value = true
    const result = await getAccounts({
      page: page.value,
      pageSize: pageSize.value,
      search: searchQuery.value,
    })
    accounts.value = result.items
    accountSummary.value = result.summary
    totalAccounts.value = result.page.total ?? result.items.length
    page.value = result.page.page ?? page.value
    pageSize.value = result.page.pageSize ?? pageSize.value

    if (accounts.value.length === 0 && totalAccounts.value > 0 && page.value > 1) {
      page.value = Math.max(1, result.page.totalPages ?? page.value - 1)
      await loadAccounts()
    }
  } finally {
    loading.value = false
    loaded.value = true
  }
}

async function handleCreate() {
  if (!createForm.value.refreshToken.trim()) return

  try {
    await createAccount({
      refreshToken: createForm.value.refreshToken,
    })
    showCreateModal.value = false
    createForm.value = { refreshToken: '' }
    await loadAccounts()
  } catch (error) {
    console.error('Failed to create account:', error)
  }
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
  if (selectedIds.value.size === 0) return

  try {
    batchDeleting.value = true
    await deleteAccounts({ ids: [...selectedIds.value] })
    selectedIds.value = new Set()
    showDeleteModal.value = false
    await loadAccounts()
  } catch (error) {
    console.error('Failed to batch delete accounts:', error)
  } finally {
    batchDeleting.value = false
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

function openConnectionTest(account: any) {
  abortConnectionTest()
  testingAccount.value = account
  connectionTestSelectedModel.value = ''
  connectionTestModelOptions.value = []
  showConnectionTestModal.value = true
  resetConnectionTest()
  void loadConnectionTestModels(account)
}

function resetConnectionTest() {
  connectionTestStatus.value = 'idle'
  connectionTestModel.value = ''
  connectionTestContent.value = ''
  connectionTestLogs.value = []
  connectionTestError.value = ''
  connectionTestStartedAt.value = ''
  connectionTestFinishedAt.value = ''
  connectionTestDurationMs.value = null
  connectionTestStartedAtMs = 0
}

function formattedDateTime(value: Date) {
  const pad = (item: number) => String(item).padStart(2, '0')
  return `${value.getFullYear()}-${pad(value.getMonth() + 1)}-${pad(value.getDate())} ${pad(value.getHours())}:${pad(value.getMinutes())}:${pad(value.getSeconds())}`
}

function formatConnectionTestDetail(value: any) {
  if (value === undefined || value === null || value === '') return ''
  if (typeof value === 'string') return value
  return JSON.stringify(value, null, 2)
}

function connectionTestRequestText(payload: any) {
  const texts = (payload?.input || [])
    .flatMap((item: any) => item?.content || [])
    .filter((item: any) => item?.type === 'input_text' && item?.text)
    .map((item: any) => item.text)
  return texts.join('\n')
}

function connectionTestLogItem(key: string, text: string, tone = 'normal', detail?: any) {
  return {
    key,
    time: formattedDateTime(new Date()).slice(11),
    text,
    tone,
    detail: formatConnectionTestDetail(detail),
  }
}

function appendConnectionTestLog(text: string, tone = 'normal', detail?: any) {
  connectionTestLogs.value = [
    ...connectionTestLogs.value,
    connectionTestLogItem(`${Date.now()}-${connectionTestLogs.value.length}`, text, tone, detail),
  ]
}

function setConnectionTestLog(key: string, text: string, tone = 'normal', detail?: any) {
  const index = connectionTestLogs.value.findIndex((item) => item.key === key)
  const next = connectionTestLogItem(key, text, tone, detail)
  if (index === -1) {
    connectionTestLogs.value = [...connectionTestLogs.value, next]
    return
  }
  connectionTestLogs.value = connectionTestLogs.value.map((item, itemIndex) =>
    itemIndex === index ? { ...next, time: item.time } : item,
  )
}

function finishConnectionTest(status: 'success' | 'error') {
  connectionTestStatus.value = status
  connectionTestFinishedAt.value = formattedDateTime(new Date())
  connectionTestDurationMs.value = Math.max(0, Date.now() - connectionTestStartedAtMs)
}

function handleConnectionTestEvent(event: any) {
  if (event.type === 'test_start') {
    connectionTestModel.value = event.model || connectionTestModel.value
    appendConnectionTestLog(`开始测试 ${connectionTestModel.value || '默认模型'}`, 'info')
    return
  }
  if (event.type === 'request') {
    setConnectionTestLog('request', '发起请求', 'info', connectionTestRequestText(event.payload))
    return
  }
  if (event.type === 'status' && event.text) {
    appendConnectionTestLog(event.text, 'info')
    return
  }
  if (event.type === 'content' && event.text) {
    connectionTestContent.value += event.text
    setConnectionTestLog('response', '接收响应内容', 'success', connectionTestContent.value)
    return
  }
  if (event.type === 'test_complete') {
    if (event.success) {
      if (!connectionTestContent.value) {
        setConnectionTestLog('response', '响应完成', 'success', '上游已完成，没有返回文本内容。')
      }
      appendConnectionTestLog('测试完成', 'success')
      finishConnectionTest('success')
    } else {
      connectionTestError.value = event.error || '测试连接失败'
      appendConnectionTestLog(connectionTestError.value, 'danger')
      finishConnectionTest('error')
    }
    return
  }
  if (event.type === 'error') {
    connectionTestError.value = event.error || '测试连接失败'
    appendConnectionTestLog(connectionTestError.value, 'danger')
    finishConnectionTest('error')
  }
}

function abortConnectionTest() {
  connectionTestAbortController?.abort()
  connectionTestAbortController = undefined
}

function connectionLogClass(tone: string) {
  if (tone === 'success') return 'text-(--cp-success-text)'
  if (tone === 'danger') return 'text-(--cp-danger-text)'
  if (tone === 'info') return 'text-(--cp-info-text)'
  return 'text-(--cp-text-secondary)'
}

async function loadConnectionTestModels(account = testingAccount.value) {
  if (!account?.id) return
  loadingConnectionTestModels.value = true
  connectionTestError.value = ''
  try {
    const result = await getAccountTestModels({ id: account.id })
    connectionTestModelOptions.value = (result.models || []).map((model: any) => ({
      label: model.label || model.id,
      value: model.id,
    }))
    connectionTestSelectedModel.value = connectionTestModelOptions.value[0]?.value || ''
    if (!connectionTestSelectedModel.value) {
      connectionTestError.value = '上游没有返回可测试模型'
    }
  } catch (error: any) {
    connectionTestError.value = error.message || '加载测试模型失败'
    connectionTestModelOptions.value = []
    connectionTestSelectedModel.value = ''
  } finally {
    loadingConnectionTestModels.value = false
  }
}

async function handleTestConnection(account = testingAccount.value) {
  if (!account?.id) return
  if (!connectionTestSelectedModel.value) {
    connectionTestError.value = '请先选择上游返回的测试模型'
    return
  }
  if (testingConnectionIds.value.has(account.id)) return
  abortConnectionTest()
  const controller = new AbortController()
  connectionTestAbortController = controller
  connectionTestStatus.value = 'running'
  connectionTestModel.value = ''
  connectionTestContent.value = ''
  connectionTestLogs.value = []
  connectionTestError.value = ''
  connectionTestDurationMs.value = null
  connectionTestModel.value = connectionTestSelectedModel.value
  connectionTestStartedAtMs = Date.now()
  connectionTestStartedAt.value = formattedDateTime(new Date())
  connectionTestFinishedAt.value = ''
  appendConnectionTestLog('准备发送测试请求', 'info')
  testingConnectionIds.value = new Set(testingConnectionIds.value).add(account.id)
  try {
    await withMinimumDuration(() =>
      testAccountConnectionStream(
        {
          id: account.id,
          modelId: connectionTestSelectedModel.value,
        },
        handleConnectionTestEvent,
        controller.signal,
      ),
    )
    if (connectionTestStatus.value === 'running') {
      connectionTestError.value = '测试连接未返回完成事件'
      appendConnectionTestLog(connectionTestError.value, 'danger')
      finishConnectionTest('error')
    }
  } catch (error: any) {
    if (error?.name !== 'AbortError') {
      connectionTestError.value = error.message || '测试连接失败'
      appendConnectionTestLog(connectionTestError.value, 'danger')
      finishConnectionTest('error')
    }
  } finally {
    const next = new Set(testingConnectionIds.value)
    next.delete(account.id)
    testingConnectionIds.value = next
    if (connectionTestAbortController === controller) {
      connectionTestAbortController = undefined
    }
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

function statusTone(status: string) {
  return statusTones[status]
}

function statusLabel(status: string) {
  return statusLabels[status]
}

function statusTextClass(status: string) {
  const tone = statusTone(status)
  if (tone === 'success') {
    return 'text-(--cp-success-text)'
  }
  if (tone === 'danger') {
    return 'text-(--cp-danger-text)'
  }
  if (tone === 'warning') {
    return 'text-(--cp-warning-text)'
  }
  if (tone === 'info') {
    return 'text-(--cp-info-text)'
  }
  return 'text-(--cp-text-secondary)'
}

function statusDotClass(status: string) {
  const tone = statusTone(status)
  if (tone === 'success') {
    return 'bg-(--cp-success)'
  }
  if (tone === 'danger') {
    return 'bg-(--cp-danger)'
  }
  if (tone === 'warning') {
    return 'bg-(--cp-warning)'
  }
  if (tone === 'info') {
    return 'bg-(--cp-info)'
  }
  return 'bg-(--cp-text-muted)'
}

function planTypeLabel(planType?: string) {
  return planType?.trim() || 'Free'
}

function planTypeClass(planType?: string) {
  const normalized = planTypeLabel(planType).toLowerCase()
  if (normalized.includes('enterprise') || normalized.includes('team')) {
    return 'bg-(--cp-bg-dark) text-(--cp-white)'
  }
  if (normalized.includes('pro')) {
    return 'bg-(--cp-info-bg) text-(--cp-info-text)'
  }
  if (normalized.includes('plus') || normalized.includes('basic')) {
    return 'bg-(--cp-normal-bg) text-(--cp-normal-text)'
  }
  return 'bg-(--cp-bg-subtle) text-(--cp-text-secondary)'
}

function accountDisplayTitle(account: any) {
  return account.label?.trim() || account.email || account.accountId || account.id
}

function accountSecondaryText(account: any) {
  const title = accountDisplayTitle(account)
  const secondary = [account.email, account.accountId, account.userId, account.id].find(
    (value) => value && value !== title,
  )
  return secondary || account.id
}

function accountInitial(account: any) {
  return accountDisplayTitle(account).slice(0, 1).toUpperCase()
}

function accountAvatarClass(account: any) {
  const palettes = [
    'bg-(--cp-info-bg) text-(--cp-info-text) shadow-[inset_0_0_0_1px_var(--cp-info-border)]',
    'bg-(--cp-success-bg) text-(--cp-success-text) shadow-[inset_0_0_0_1px_var(--cp-success-border)]',
    'bg-(--cp-normal-bg) text-(--cp-normal-text) shadow-[inset_0_0_0_1px_var(--cp-normal-border)]',
    'bg-(--cp-warning-bg) text-(--cp-warning-text) shadow-[inset_0_0_0_1px_var(--cp-warning-border)]',
  ]
  const key = account.id || account.email || accountDisplayTitle(account)
  const hash = [...key].reduce((sum, char) => sum + char.charCodeAt(0), 0)
  return palettes[hash % palettes.length]
}

function formatCount(value: number) {
  return value.toLocaleString('zh-CN')
}

function toggleSelection(accountId: string) {
  if (selectedIds.value.has(accountId)) {
    selectedIds.value.delete(accountId)
  } else {
    selectedIds.value.add(accountId)
  }
}

function toggleExpanded(accountId: string) {
  const next = new Set(expandedAccountIds.value)
  if (next.has(accountId)) {
    next.delete(accountId)
  } else {
    next.add(accountId)
  }
  expandedAccountIds.value = next
}

function toggleAll() {
  if (allSelected.value) {
    accounts.value.forEach((account) => selectedIds.value.delete(account.id))
  } else {
    accounts.value.forEach((account) => selectedIds.value.add(account.id))
  }
}

function handlePageChange(nextPage: number) {
  page.value = nextPage
  void loadAccounts()
}

function handlePageSizeChange(nextPageSize: number) {
  pageSize.value = nextPageSize
  page.value = 1
  void loadAccounts()
}

function quotaWindows(account: any) {
  return account.quota.windows as any[]
}

function quotaWindowsByGroup(account: any, group: string) {
  return quotaWindows(account).filter((window) => window.group === group)
}

function quotaWindowPercent(window?: any) {
  return Math.max(0, Math.min(window?.usedPercent ?? 0, 100))
}

function quotaWindowBarWidth(window?: any) {
  return `${quotaWindowPercent(window)}%`
}

function quotaWindowBarStyle(window?: any) {
  const percent = quotaWindowPercent(window)
  return {
    width: `${percent}%`,
    minWidth: percent > 0 ? '6px' : '0',
  }
}

function quotaWindowBarClass(window?: any) {
  if (window?.usedPercent === null || window?.usedPercent === undefined) {
    return 'bg-(--cp-default-border-hover)'
  }
  if (window.usedPercent >= 95) {
    return 'bg-(--cp-danger)'
  }
  if (window.usedPercent >= 80) {
    return 'bg-(--cp-warning)'
  }
  return 'bg-(--cp-success)'
}

function overviewIconClass(tone: string) {
  if (tone === 'success') {
    return 'bg-(--cp-success-bg) text-(--cp-success-text)'
  }
  if (tone === 'warning') {
    return 'bg-(--cp-warning-bg) text-(--cp-warning-text)'
  }
  if (tone === 'danger') {
    return 'bg-(--cp-danger-bg) text-(--cp-danger-text)'
  }
  return 'bg-(--cp-info-bg) text-(--cp-info-text)'
}

onMounted(() => {
  loadAccounts()
})

watch(searchQuery, () => {
  page.value = 1
  if (searchTimer) {
    window.clearTimeout(searchTimer)
  }
  searchTimer = window.setTimeout(() => {
    void loadAccounts()
  }, 250)
})

watch(showConnectionTestModal, (open) => {
  if (!open) {
    abortConnectionTest()
  }
})

onBeforeUnmount(() => {
  abortConnectionTest()
  if (searchTimer) {
    window.clearTimeout(searchTimer)
  }
})
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col overflow-hidden">
    <header class="flex h-17 shrink-0 items-start justify-between">
      <div>
        <h1 class="mt-0 text-[34px] leading-[1.15] font-extrabold mb-0 text-(--cp-text-primary)">
          账号管理
        </h1>
        <p class="mt-2.5 text-[15px] leading-[1.15] font-semibold mb-0 text-(--cp-text-secondary)">
          维护 Codex 账号池，快速确认可用性、配额与连接状态。
        </p>
      </div>
    </header>

    <div class="mt-5 grid shrink-0 grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-4">
      <BaseCard
        v-for="item in overviewItems"
        :key="item.label"
        as="article"
        :padded="false"
        class="h-24"
      >
        <div class="flex h-full items-stretch justify-between gap-3 px-5 py-3">
          <div class="flex min-w-0 flex-col justify-between">
            <p class="m-0 text-[12px] leading-none font-[760] text-(--cp-text-secondary)">
              {{ item.label }}
            </p>
            <strong
              class="block font-mono text-[26px] leading-none font-[820] text-(--cp-text-primary)"
            >
              {{ item.value }}
            </strong>
            <p class="m-0 truncate text-[12px] leading-none font-[650] text-(--cp-text-muted)">
              {{ item.caption }}
            </p>
          </div>
          <span
            class="inline-flex size-9 shrink-0 items-center justify-center self-start rounded-lg"
            :class="overviewIconClass(item.tone)"
          >
            <component :is="item.icon" class="size-4.5" />
          </span>
        </div>
      </BaseCard>
    </div>

    <BaseCard
      v-loading="initialLoading"
      :padded="false"
      class="mt-4 flex min-h-0 flex-1 flex-col"
      header-class="px-4 pt-4 pb-2 md:px-5"
      body-class="flex min-h-0 flex-1 px-4 pb-3 md:px-5"
    >
      <template #header>
        <div class="flex flex-wrap items-center justify-between gap-3">
          <div class="flex min-w-0 flex-1 flex-wrap items-center gap-3">
            <BaseInput
              v-model="searchQuery"
              placeholder="搜索邮箱或 ID..."
              class="w-80 max-w-full [--cp-input-current-bg:var(--cp-input-soft-bg)] [--cp-input-current-bg-hover:var(--cp-input-soft-bg-hover)]"
            >
              <template #prefix>
                <Search class="size-4.5 text-(--cp-text-tertiary)" />
              </template>
            </BaseInput>

            <BaseButton
              v-if="selectedIds.size > 0"
              variant="danger"
              :disabled="batchDeleting"
              @click="showDeleteModal = true"
            >
              <Trash2 class="size-4" />
              删除选中 ({{ selectedIds.size }})
            </BaseButton>
          </div>

          <div class="flex shrink-0 items-center gap-2">
            <BaseButton variant="primary" @click="showCreateModal = true">
              <Plus class="size-4" />
              添加账号
            </BaseButton>
          </div>
        </div>
      </template>

      <template #body>
        <BaseTable
          class="min-h-0 flex-1"
          :columns="accountColumns"
          :rows="filteredAccounts"
          :loading="loading"
          :selected-row-keys="selectedRowKeys"
          :expanded-row-keys="[...expandedAccountIds]"
          :pagination="accountPagination"
          empty-text="暂无账号数据"
          @page-change="handlePageChange"
          @page-size-change="handlePageSizeChange"
        >
          <template #expander="{ row }">
            <button
              type="button"
              class="inline-flex size-6 cursor-pointer items-center justify-center rounded-md border-0 bg-transparent text-(--cp-text-secondary) transition hover:bg-(--cp-default-bg-hover) hover:text-(--cp-text-primary)"
              :title="expandedAccountIds.has(row.id) ? '收起统计' : '展开统计'"
              @click.stop="toggleExpanded(row.id)"
            >
              <ChevronDown
                class="size-3.5 transition-transform"
                :class="expandedAccountIds.has(row.id) ? '' : '-rotate-90'"
              />
            </button>
          </template>

          <template #header-selection>
            <BaseCheckbox
              :model-value="allSelected"
              :indeterminate="indeterminate"
              label="选择当前页账号"
              @update:model-value="toggleAll"
            />
          </template>

          <template #selection="{ row }">
            <BaseCheckbox
              :model-value="selectedIds.has(row.id)"
              label="选择账号"
              @update:model-value="toggleSelection(row.id)"
            />
          </template>

          <template #identity="{ row }">
            <div class="flex min-w-0 items-center gap-3">
              <span
                class="inline-flex size-9 shrink-0 items-center justify-center rounded-lg text-[13px] font-[820]"
                :class="accountAvatarClass(row)"
              >
                {{ accountInitial(row) }}
              </span>
              <div class="min-w-0">
                <div class="truncate text-[14px] font-[760] text-(--cp-text-primary)">
                  {{ accountDisplayTitle(row) }}
                </div>
                <div class="mt-0.5 truncate font-mono text-[11px] text-(--cp-text-muted)">
                  {{ accountSecondaryText(row) }}
                </div>
              </div>
            </div>
          </template>

          <template #status="{ row }">
            <span
              class="inline-flex min-w-16 items-center gap-1.5 text-[12px] leading-none font-[650]"
              :class="statusTextClass(row.status)"
            >
              <span class="size-1.5 rounded-full" :class="statusDotClass(row.status)" />
              <span>{{ statusLabel(row.status) }}</span>
            </span>
          </template>

          <template #planType="{ row }">
            <span
              class="inline-flex items-center rounded-full px-2.5 py-1 text-[12px] font-[760] capitalize"
              :class="planTypeClass(row.planType)"
            >
              {{ planTypeLabel(row.planType) }}
            </span>
          </template>

          <template #usage="{ row }">
            <div
              v-if="quotaWindows(row).length > 0"
              class="flex w-full max-w-31 min-w-0 flex-col gap-2 whitespace-normal py-1.5"
            >
              <div
                v-for="window in quotaWindows(row)"
                :key="window.key"
                class="min-w-0 border-b border-slate-200/70 pb-2 last:border-b-0 last:pb-0"
              >
                <div
                  class="mb-1.5 flex items-center justify-between gap-2 text-[11px] leading-none font-[760]"
                >
                  <span class="truncate text-(--cp-text-secondary)">
                    {{ window.labelDisplay }}
                  </span>
                  <span class="shrink-0 font-mono text-(--cp-text-primary)">
                    {{ window.usedPercentDisplay }}
                  </span>
                </div>
                <div class="h-1 w-full overflow-hidden rounded-full bg-(--cp-default-border)">
                  <div
                    class="h-full rounded-full transition-[width,background-color] duration-200"
                    :class="quotaWindowBarClass(window)"
                    :style="quotaWindowBarStyle(window)"
                  />
                </div>
              </div>
            </div>
            <span v-else class="text-(--cp-text-muted)">-</span>
          </template>

          <template #updatedAt="{ row }">
            <span class="text-(--cp-text-secondary)">
              {{ row.updatedAtDisplay }}
            </span>
          </template>

          <template #accessTokenExpiresAt="{ row }">
            <span class="text-(--cp-text-secondary)">
              {{ row.accessTokenExpiresAtDisplay || '—' }}
            </span>
          </template>

          <template #actions="{ row }">
            <div class="relative flex items-center justify-end gap-1">
              <BaseButton
                icon-only
                variant="ghost"
                size="sm"
                title="编辑账号"
                @click.stop="openEditAccount(row)"
              >
                <Pencil class="size-3.5" />
              </BaseButton>

              <BaseButton
                icon-only
                variant="ghost"
                size="sm"
                title="删除账号"
                :disabled="deletingAccount"
                @click.stop="requestDeleteAccount(row)"
              >
                <Trash2 class="size-3.5 text-(--cp-danger)" />
              </BaseButton>

              <BasePopover placement="bottom-end" width="160px">
                <template #trigger="{ open }">
                  <BaseButton icon-only variant="ghost" size="sm" title="更多操作" :active="open">
                    <MoreHorizontal class="size-4" />
                  </BaseButton>
                </template>

                <template #default="{ close }">
                  <button
                    type="button"
                    class="flex h-8.5 w-full items-center gap-2 rounded-(--cp-input-radius-small) border-0 bg-transparent px-3 text-left text-[13px] leading-none font-[650] text-(--cp-text-primary) transition-colors hover:bg-(--cp-default-bg-hover) disabled:cursor-not-allowed disabled:text-(--cp-disabled-text)"
                    :disabled="testingConnectionIds.has(row.id)"
                    @click.stop="(close(), openConnectionTest(row))"
                  >
                    <Wifi class="size-3.5 text-(--cp-text-muted)" />
                    测试连接
                  </button>
                  <button
                    type="button"
                    class="flex h-8.5 w-full items-center gap-2 rounded-(--cp-input-radius-small) border-0 bg-transparent px-3 text-left text-[13px] leading-none font-[650] text-(--cp-text-primary) transition-colors hover:bg-(--cp-default-bg-hover) disabled:cursor-not-allowed disabled:text-(--cp-disabled-text)"
                    :disabled="refreshingAccountIds.has(row.id)"
                    @click.stop="(close(), handleRefresh(row.id))"
                  >
                    <RefreshCw class="size-3.5 text-(--cp-text-muted)" />
                    刷新 token
                  </button>
                  <button
                    type="button"
                    class="flex h-8.5 w-full items-center gap-2 rounded-(--cp-input-radius-small) border-0 bg-transparent px-3 text-left text-[13px] leading-none font-[650] text-(--cp-text-primary) transition-colors hover:bg-(--cp-default-bg-hover) disabled:cursor-not-allowed disabled:text-(--cp-disabled-text)"
                    :disabled="updatingStatusAccountIds.has(row.id)"
                    @click.stop="(close(), handleToggleSchedule(row))"
                  >
                    <Power class="size-3.5 text-(--cp-text-muted)" />
                    {{ scheduleActionLabel(row) }}
                  </button>
                </template>
              </BasePopover>
            </div>
          </template>

          <template #expanded="{ row }">
            <div class="grid gap-3 p-4 lg:grid-cols-[1.05fr_2.45fr]">
              <section class="rounded-lg bg-(--cp-bg-surface) p-4 shadow-(--cp-shadow-control)">
                <div class="mb-3 flex items-center justify-between gap-3">
                  <div>
                    <h3 class="m-0 text-[14px] font-[760] text-(--cp-text-primary)">账号额度</h3>
                    <p class="m-0 mt-1 text-[12px] font-[620] text-(--cp-text-secondary)">
                      Codex 额度 · 套餐: {{ row.planType || 'Free' }} · 最近刷新:
                      {{ row.quota.refreshedAtDisplay }}
                    </p>
                  </div>
                  <BaseButton
                    icon-only
                    variant="ghost"
                    size="sm"
                    title="刷新额度"
                    :loading="refreshingQuotaAccountIds.has(row.id)"
                    @click="handleRefreshQuota(row.id)"
                  >
                    <RefreshCw class="size-3.5" />
                  </BaseButton>
                </div>

                <div class="grid gap-3">
                  <div
                    v-for="window in quotaWindowsByGroup(row, 'monthly')"
                    :key="window.key"
                    class="rounded-lg bg-(--cp-bg-subtle) p-2"
                  >
                    <div class="flex items-center justify-between gap-3 text-[12px] font-[720]">
                      <span class="text-(--cp-text-secondary)">{{ window.labelDisplay }}</span>
                      <span class="text-(--cp-text-primary)">{{ window.usedPercentDisplay }}</span>
                    </div>
                    <div class="mt-2 h-2 overflow-hidden rounded-full bg-(--cp-bg-tertiary)">
                      <div
                        class="h-full rounded-full bg-(--cp-info)"
                        :style="{ width: quotaWindowBarWidth(window) }"
                      />
                    </div>
                    <div
                      class="mt-3 flex flex-wrap justify-between gap-x-3 gap-y-1 text-[12px] font-[620] text-(--cp-text-secondary)"
                    >
                      <span>重置时间: {{ window.resetAtDisplay }}</span>
                      <span>窗口已用: {{ window.windowUsedDisplay }}</span>
                    </div>
                  </div>

                  <div
                    v-if="quotaWindowsByGroup(row, 'shortTerm').length > 0"
                    class="grid gap-2 sm:grid-cols-2"
                  >
                    <div
                      v-for="window in quotaWindowsByGroup(row, 'shortTerm')"
                      :key="window.key"
                      class="rounded-lg bg-(--cp-bg-subtle) p-2"
                    >
                      <div class="flex items-center justify-between gap-3 text-[12px] font-[720]">
                        <span class="text-(--cp-text-secondary)">{{ window.labelDisplay }}</span>
                        <span class="text-(--cp-text-primary)">{{
                          window.usedPercentDisplay
                        }}</span>
                      </div>
                      <div class="mt-2 h-2 overflow-hidden rounded-full bg-(--cp-bg-tertiary)">
                        <div
                          class="h-full rounded-full bg-(--cp-info)"
                          :style="{ width: quotaWindowBarWidth(window) }"
                        />
                      </div>
                      <div
                        class="mt-3 flex flex-col gap-1 text-[12px] font-[620] text-(--cp-text-secondary)"
                      >
                        <span>重置时间: {{ window.resetAtDisplay }}</span>
                        <span>窗口已用: {{ window.windowUsedDisplay }}</span>
                      </div>
                    </div>
                  </div>

                  <div
                    v-for="window in quotaWindowsByGroup(row, 'other')"
                    :key="window.key"
                    class="rounded-lg bg-(--cp-bg-subtle) p-2"
                  >
                    <div class="flex items-center justify-between gap-3 text-[12px] font-[720]">
                      <span class="text-(--cp-text-secondary)">{{ window.labelDisplay }}</span>
                      <span class="text-(--cp-text-primary)">{{ window.usedPercentDisplay }}</span>
                    </div>
                    <div class="mt-2 h-2 overflow-hidden rounded-full bg-(--cp-bg-tertiary)">
                      <div
                        class="h-full rounded-full bg-(--cp-info)"
                        :style="{ width: quotaWindowBarWidth(window) }"
                      />
                    </div>
                    <div
                      class="mt-3 flex flex-wrap justify-between gap-x-3 gap-y-1 text-[12px] font-[620] text-(--cp-text-secondary)"
                    >
                      <span>重置时间: {{ window.resetAtDisplay }}</span>
                      <span>窗口已用: {{ window.windowUsedDisplay }}</span>
                    </div>
                  </div>
                </div>
              </section>

              <section
                class="grid gap-4 rounded-lg bg-(--cp-bg-surface) p-4 shadow-(--cp-shadow-control) xl:grid-cols-[0.52fr_1.48fr]"
              >
                <div>
                  <h3 class="m-0 mb-3 text-[14px] font-[760] text-(--cp-text-primary)">
                    Token 结构
                  </h3>
                  <div class="grid gap-2">
                    <div
                      class="flex items-center justify-between rounded-lg bg-(--cp-success-bg) px-3 py-2"
                    >
                      <span class="text-[12px] font-bold text-(--cp-success-text)"
                        >输入 Tokens</span
                      >
                      <strong class="font-mono text-[13px] text-(--cp-text-primary)">
                        {{ row.usage.inputTokensDisplay }}
                      </strong>
                    </div>
                    <div
                      class="flex items-center justify-between rounded-lg bg-(--cp-warning-bg) px-3 py-2"
                    >
                      <span class="text-[12px] font-bold text-(--cp-warning-text)"
                        >输出 Tokens</span
                      >
                      <strong class="font-mono text-[13px] text-(--cp-text-primary)">
                        {{ row.usage.outputTokensDisplay }}
                      </strong>
                    </div>
                    <div
                      class="flex items-center justify-between rounded-lg bg-(--cp-normal-bg) px-3 py-2"
                    >
                      <span class="text-[12px] font-bold text-(--cp-normal-text)">缓存 Tokens</span>
                      <strong class="font-mono text-[13px] text-(--cp-text-primary)">
                        {{ row.usage.cachedTokensDisplay }}
                      </strong>
                    </div>
                    <div
                      class="flex items-center justify-between rounded-lg bg-(--cp-info-bg) px-3 py-2"
                    >
                      <span class="text-[12px] font-bold text-(--cp-info-text)">创建</span>
                      <strong class="font-mono text-[13px] text-(--cp-text-primary)">
                        {{ row.usage.createdTokensDisplay }}
                      </strong>
                    </div>
                    <div
                      class="flex items-center justify-between rounded-lg bg-(--cp-info-bg) px-3 py-2"
                    >
                      <span class="text-[12px] font-bold text-(--cp-info-text)">读取</span>
                      <strong class="font-mono text-[13px] text-(--cp-text-primary)">
                        {{ row.usage.readTokensDisplay }}
                      </strong>
                    </div>
                  </div>
                </div>

                <div
                  class="min-w-0 pt-4 shadow-[inset_0_1px_0_rgba(216,224,234,0.42)] xl:pt-0 xl:pl-4 xl:shadow-[inset_1px_0_0_rgba(216,224,234,0.42)]"
                >
                  <div class="mb-3 flex items-center justify-between">
                    <h3 class="m-0 text-[14px] font-[760] text-(--cp-text-primary)">
                      模型使用排行
                    </h3>
                  </div>

                  <div
                    class="grid grid-cols-[1.2fr_0.7fr_0.8fr_1fr_1fr_1fr_1fr_1fr_1.4fr] gap-3 pb-2 text-[11px] font-[760] text-(--cp-text-muted) shadow-[inset_0_-1px_0_rgba(216,224,234,0.42)]"
                  >
                    <span>模型</span>
                    <span>调用</span>
                    <span>成功率</span>
                    <span>输入</span>
                    <span>输出</span>
                    <span>缓存</span>
                    <span>总TOKEN</span>
                    <span>总花费</span>
                    <span>最近请求时间</span>
                  </div>
                  <div
                    v-if="row.usage.models.length === 0"
                    class="pt-3 text-[12px] font-[650] text-(--cp-text-muted)"
                  >
                    -
                  </div>
                  <template v-else>
                    <div
                      v-for="model in row.usage.models"
                      :key="model.model"
                      class="grid grid-cols-[1.2fr_0.7fr_0.8fr_1fr_1fr_1fr_1fr_1fr_1.4fr] gap-3 pt-3 text-[12px] font-[650] text-(--cp-text-primary)"
                    >
                      <span class="truncate">{{ model.model }}</span>
                      <span>{{ model.requestCountDisplay }}</span>
                      <span class="text-(--cp-warning-text)">{{ model.successRateDisplay }}</span>
                      <span>{{ model.inputTokensDisplay }}</span>
                      <span>{{ model.outputTokensDisplay }}</span>
                      <span>{{ model.cachedTokensDisplay }}</span>
                      <span>{{ model.totalTokensDisplay }}</span>
                      <span>{{ model.totalCostUsdDisplay }}</span>
                      <span>{{ model.lastUsedAtDisplay }}</span>
                    </div>
                  </template>
                </div>
              </section>
            </div>
          </template>
        </BaseTable>
      </template>
    </BaseCard>

    <BaseModal
      v-model="showConnectionTestModal"
      title="测试连接"
      description="验证账号令牌、ChatGPT 账号 ID 与 Codex 模型端点是否可用。"
      variant="info"
      width="720px"
    >
      <div v-if="testingAccount" class="flex flex-col gap-4">
        <section
          class="flex items-center justify-between gap-4 rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-3"
        >
          <div class="flex min-w-0 items-center gap-3">
            <span
              class="inline-flex size-10 shrink-0 items-center justify-center rounded-lg text-[15px] font-[820]"
              :class="accountAvatarClass(testingAccount)"
            >
              {{ accountInitial(testingAccount) }}
            </span>
            <div class="min-w-0">
              <p class="m-0 truncate text-[14px] font-[760] text-(--cp-text-primary)">
                {{ accountDisplayTitle(testingAccount) }}
              </p>
              <p class="mt-1 mb-0 truncate text-[12px] font-[650] text-(--cp-text-secondary)">
                {{ accountSecondaryText(testingAccount) }}
              </p>
            </div>
          </div>
          <span
            class="inline-flex h-7 shrink-0 items-center rounded-full px-2.5 text-[12px] font-[760]"
            :class="statusTextClass(testingAccount.status)"
          >
            {{ statusLabel(testingAccount.status) || testingAccount.status }}
          </span>
        </section>

        <section class="rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-3">
          <div class="grid gap-2">
            <label class="text-[12px] font-[760] text-(--cp-text-muted)">测试模型</label>
            <BaseSelect
              v-model="connectionTestSelectedModel"
              :options="connectionTestModelOptions"
              :disabled="connectionTestStatus === 'running' || loadingConnectionTestModels"
              :placeholder="loadingConnectionTestModels ? '加载模型中...' : '选择上游模型'"
              empty-text="上游没有返回模型"
            />
          </div>
        </section>

        <section class="rounded-(--cp-card-radius) bg-(--cp-bg-subtle) p-4">
          <div class="flex items-start justify-between gap-4">
            <div class="flex min-w-0 items-start gap-3">
              <span
                class="inline-flex size-10 shrink-0 items-center justify-center rounded-lg"
                :class="connectionTestStatusView.badge"
              >
                <component
                  :is="connectionTestStatusView.icon"
                  class="size-5"
                  :class="[
                    connectionTestStatusView.iconClass,
                    connectionTestStatus === 'running' ? 'animate-pulse' : '',
                  ]"
                />
              </span>
              <div class="min-w-0">
                <p class="m-0 text-[16px] font-[780] text-(--cp-text-primary)">
                  {{ connectionTestStatusView.label }}
                </p>
                <p
                  class="mt-1.5 mb-0 text-[13px] leading-[1.5] font-[650] text-(--cp-text-secondary)"
                >
                  {{ connectionTestStatusView.description }}
                </p>
              </div>
            </div>
            <span
              class="inline-flex h-7 shrink-0 items-center rounded-full px-2.5 text-[12px] font-[760]"
              :class="connectionTestStatusView.badge"
            >
              {{ connectionTestStatus === 'running' ? '检测中' : connectionTestStatusView.label }}
            </span>
          </div>

          <div class="mt-4 grid gap-3 sm:grid-cols-3">
            <div class="rounded-lg bg-(--cp-bg-surface) px-3 py-2.5">
              <p class="m-0 text-[11px] font-[760] text-(--cp-text-muted)">开始时间</p>
              <p class="mt-1.5 mb-0 font-mono text-[12px] font-[650] text-(--cp-text-primary)">
                {{ connectionTestStartedAt || '-' }}
              </p>
            </div>
            <div class="rounded-lg bg-(--cp-bg-surface) px-3 py-2.5">
              <p class="m-0 text-[11px] font-[760] text-(--cp-text-muted)">完成时间</p>
              <p class="mt-1.5 mb-0 font-mono text-[12px] font-[650] text-(--cp-text-primary)">
                {{ connectionTestFinishedAt || '-' }}
              </p>
            </div>
            <div class="rounded-lg bg-(--cp-bg-surface) px-3 py-2.5">
              <p class="m-0 text-[11px] font-[760] text-(--cp-text-muted)">响应耗时</p>
              <p class="mt-1.5 mb-0 font-mono text-[12px] font-[650] text-(--cp-text-primary)">
                {{ connectionTestDurationMs !== null ? `${connectionTestDurationMs}ms` : '-' }}
              </p>
            </div>
          </div>

          <div class="mt-3 rounded-lg bg-(--cp-bg-surface) px-3 py-2.5">
            <p class="m-0 text-[11px] font-[760] text-(--cp-text-muted)">测试模型</p>
            <p
              class="mt-1.5 mb-0 truncate font-mono text-[12px] font-[650] text-(--cp-text-primary)"
              :title="connectionTestModel || '-'"
            >
              {{ connectionTestModel || '-' }}
            </p>
          </div>

          <div class="mt-3 rounded-lg bg-(--cp-bg-surface) px-3 py-2.5">
            <p class="m-0 text-[11px] font-[760] text-(--cp-text-muted)">事件轨迹</p>
            <BaseScrollbar max-height="260px" view-class="pt-2 pr-2">
              <div
                v-if="connectionTestLogs.length === 0"
                class="text-[12px] font-[650] text-(--cp-text-muted)"
              >
                -
              </div>
              <div v-else class="flex flex-col gap-1.5">
                <div
                  v-for="item in connectionTestLogs"
                  :key="item.key"
                  class="grid grid-cols-[54px_minmax(0,1fr)] gap-2 text-[12px] leading-[1.45] font-[650]"
                >
                  <span class="font-mono text-(--cp-text-muted)">{{ item.time }}</span>
                  <div class="min-w-0">
                    <p class="m-0 break-words" :class="connectionLogClass(item.tone)">
                      {{ item.text }}
                    </p>
                    <div v-if="item.detail" class="mt-2 rounded-lg bg-(--cp-bg-subtle) px-3 py-2">
                      <BaseScrollbar max-height="138px" view-class="pr-2">
                        <pre
                          class="m-0 whitespace-pre-wrap break-words font-mono text-[11px] leading-[1.6] font-[620] text-(--cp-text-primary)"
                          >{{ item.detail }}</pre
                        >
                      </BaseScrollbar>
                    </div>
                  </div>
                </div>
              </div>
            </BaseScrollbar>
          </div>

          <div v-if="connectionTestError" class="mt-3 rounded-lg bg-(--cp-danger-bg) px-3 py-2.5">
            <p class="m-0 text-[11px] font-[760] text-(--cp-danger-text)">错误信息</p>
            <BaseScrollbar max-height="118px" view-class="pt-1.5 pr-2">
              <p
                class="m-0 break-words text-[12px] leading-[1.55] font-[650] text-(--cp-danger-text)"
              >
                {{ connectionTestError }}
              </p>
            </BaseScrollbar>
          </div>
        </section>
      </div>

      <template #footer>
        <BaseButton variant="ghost" @click="showConnectionTestModal = false">关闭</BaseButton>
        <BaseButton
          variant="primary"
          :loading="connectionTestStatus === 'running'"
          :disabled="!testingAccount || loadingConnectionTestModels || !connectionTestSelectedModel"
          @click="handleTestConnection()"
        >
          {{ connectionTestLogs.length > 0 || connectionTestError ? '重新测试' : '开始测试' }}
        </BaseButton>
      </template>
    </BaseModal>

    <BaseModal
      v-model="showCreateModal"
      title="添加账号"
      description="粘贴 Refresh Token 后创建一个可参与调度的 Codex 账号。"
      variant="info"
      width="540px"
    >
      <div class="flex flex-col gap-4">
        <div>
          <label class="block text-[13px] font-medium text-(--cp-text-secondary) mb-2">
            Refresh Token <span class="text-(--cp-danger)">*</span>
          </label>
          <BaseInput
            v-model="createForm.refreshToken"
            placeholder="粘贴 Refresh Token..."
            type="password"
          />
        </div>
      </div>

      <template #footer>
        <BaseButton variant="ghost" @click="showCreateModal = false"> 取消 </BaseButton>
        <BaseButton
          variant="primary"
          :disabled="!createForm.refreshToken.trim()"
          @click="handleCreate"
        >
          添加
        </BaseButton>
      </template>
    </BaseModal>

    <BaseModal
      v-model="showEditModal"
      title="编辑账号"
      description="更新账号元信息、套餐和调度状态。"
      variant="info"
      width="680px"
    >
      <div class="grid gap-4 sm:grid-cols-2">
        <div class="sm:col-span-2">
          <label class="mb-2 block text-[13px] font-medium text-(--cp-text-secondary)">
            内部 ID
          </label>
          <div
            class="flex h-10 items-center truncate rounded-lg bg-(--cp-bg-subtle) px-3 font-mono text-[12px] font-[650] text-(--cp-text-muted)"
          >
            {{ editingAccount?.id || '-' }}
          </div>
        </div>

        <div>
          <label class="mb-2 block text-[13px] font-medium text-(--cp-text-secondary)">
            备注标签
          </label>
          <BaseInput v-model="editForm.label" placeholder="例如：主账号 / 备用账号" />
        </div>

        <div>
          <label class="mb-2 block text-[13px] font-medium text-(--cp-text-secondary)">
            邮箱
          </label>
          <BaseInput v-model="editForm.email" placeholder="account@example.com" />
        </div>

        <div>
          <label class="mb-2 block text-[13px] font-medium text-(--cp-text-secondary)">
            ChatGPT 账号 ID
          </label>
          <BaseInput v-model="editForm.accountId" placeholder="chatgpt account id" />
        </div>

        <div>
          <label class="mb-2 block text-[13px] font-medium text-(--cp-text-secondary)">
            用户 ID
          </label>
          <BaseInput v-model="editForm.userId" placeholder="user id" />
        </div>

        <div>
          <label class="mb-2 block text-[13px] font-medium text-(--cp-text-secondary)">
            套餐
          </label>
          <BaseInput v-model="editForm.planType" placeholder="free / plus / pro / team" />
        </div>

        <div>
          <label class="mb-2 block text-[13px] font-medium text-(--cp-text-secondary)">
            状态
          </label>
          <BaseSelect v-model="editStatusModel" :options="editableStatusOptions" />
        </div>
      </div>

      <template #footer>
        <BaseButton variant="ghost" @click="showEditModal = false"> 取消 </BaseButton>
        <BaseButton variant="primary" :loading="savingAccount" @click="handleSaveAccount">
          保存
        </BaseButton>
      </template>
    </BaseModal>

    <BaseConfirmModal
      v-model="showDeleteModal"
      title="确认删除"
      description="删除后该账号将不再参与调度，此操作不可撤销。"
      :message="`确定要删除选中的 ${selectedIds.size} 个账号吗？此操作不可撤销。`"
      variant="danger"
      confirm-text="确认删除"
      :loading="batchDeleting"
      width="480px"
      @confirm="handleBatchDelete"
    />

    <BaseConfirmModal
      v-model="showSingleDeleteModal"
      title="删除账号"
      description="删除后该账号将不再参与调度，此操作不可撤销。"
      :message="`确定要删除 ${pendingDeleteAccount?.email || pendingDeleteAccount?.accountId || pendingDeleteAccount?.id || '该账号'} 吗？`"
      variant="danger"
      confirm-text="确认删除"
      :loading="deletingAccount"
      width="480px"
      @confirm="handleDelete"
    />
  </div>
</template>
