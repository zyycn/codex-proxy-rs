<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { Plus, RefreshCw, Trash2, Download, Upload, Search } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseTable from '@/components/base/BaseTable.vue'
import AppTopbar from '@/layout/components/AppTopbar.vue'
import { withMinimumDuration } from '@/utils/async'

import type { Account } from '@/api'
import {
  batchDeleteAccounts,
  createAccount,
  deleteAccount,
  getAccounts,
  refreshAccount,
} from '@/api'

const loading = ref(true)
const accounts = ref<Account[]>([])
const totalAccounts = ref(0)
const page = ref(1)
const pageSize = ref(20)
const searchQuery = ref('')
const selectedIds = ref<Set<string>>(new Set())
const showCreateModal = ref(false)
const showDeleteModal = ref(false)
const showSingleDeleteModal = ref(false)
const pendingDeleteAccount = ref<Account | null>(null)
const refreshingList = ref(false)
const refreshingAccountIds = ref<Set<string>>(new Set())
const deletingAccount = ref(false)
const batchDeleting = ref(false)
let searchTimer: number | undefined

const createForm = ref({
  refreshToken: '',
})

const accountColumns = [
  { key: 'selection', label: '', width: '56px', align: 'left' as const },
  { key: 'identity', label: '邮箱', flex: 2.6, minWidth: '260px' },
  { key: 'status', label: '状态', flex: 0.8 },
  { key: 'planType', label: '套餐', flex: 0.8 },
  { key: 'requests', label: '请求数', flex: 0.9 },
  { key: 'tokens', label: '总 Tokens', flex: 1 },
  { key: 'updatedAt', label: '最后使用', flex: 1.2 },
  { key: 'accessTokenExpiresAt', label: '过期时间', flex: 1.2 },
  { key: 'actions', label: '操作', width: '104px', align: 'left' as const },
]

const statusLabels: Record<Account['status'], string> = {
  active: '正常',
  expired: '已过期',
  disabled: '已禁用',
  banned: '已封禁',
  quota_exhausted: '配额耗尽',
  refreshing: '刷新中',
}

const statusTones: Record<Account['status'], 'success' | 'danger' | 'warning' | 'info' | 'normal'> =
  {
    active: 'success',
    expired: 'warning',
    disabled: 'normal',
    banned: 'danger',
    quota_exhausted: 'warning',
    refreshing: 'info',
  }

const filteredAccounts = computed(() => accounts.value)

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

async function loadAccounts() {
  try {
    loading.value = true
    const result = await getAccounts({
      page: page.value,
      pageSize: pageSize.value,
      search: searchQuery.value,
    })
    accounts.value = result.items
    totalAccounts.value = result.page.total ?? result.items.length
    page.value = result.page.page ?? page.value
    pageSize.value = result.page.pageSize ?? pageSize.value

    if (accounts.value.length === 0 && totalAccounts.value > 0 && page.value > 1) {
      page.value = Math.max(1, result.page.totalPages ?? page.value - 1)
      await loadAccounts()
    }
  } finally {
    loading.value = false
  }
}

async function refreshAccounts() {
  if (refreshingList.value || loading.value) return
  refreshingList.value = true
  try {
    await withMinimumDuration(loadAccounts)
  } finally {
    refreshingList.value = false
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

function requestDeleteAccount(account: Account) {
  pendingDeleteAccount.value = account
  showSingleDeleteModal.value = true
}

async function handleDelete() {
  const accountId = pendingDeleteAccount.value?.id
  if (!accountId) return

  try {
    deletingAccount.value = true
    await deleteAccount(accountId)
    showSingleDeleteModal.value = false
    pendingDeleteAccount.value = null
    await loadAccounts()
  } catch (error) {
    console.error('Failed to delete account:', error)
  } finally {
    deletingAccount.value = false
  }
}

async function handleBatchDelete() {
  if (selectedIds.value.size === 0) return

  try {
    batchDeleting.value = true
    await batchDeleteAccounts([...selectedIds.value])
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
      await refreshAccount(accountId)
      await loadAccounts()
    })
  } catch (error) {
    console.error('Failed to refresh account:', error)
  } finally {
    const next = new Set(refreshingAccountIds.value)
    next.delete(accountId)
    refreshingAccountIds.value = next
  }
}

function statusTone(status: Account['status']) {
  return statusTones[status]
}

function statusLabel(status: Account['status']) {
  return statusLabels[status]
}

function toggleSelection(accountId: string) {
  if (selectedIds.value.has(accountId)) {
    selectedIds.value.delete(accountId)
  } else {
    selectedIds.value.add(accountId)
  }
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

function formatDate(dateStr?: string): string {
  if (!dateStr) return '—'
  const date = new Date(dateStr)
  if (Number.isNaN(date.getTime())) return dateStr

  return new Intl.DateTimeFormat('zh-CN', {
    timeZone: 'Asia/Shanghai',
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  })
    .format(date)
    .replaceAll('/', '-')
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

onBeforeUnmount(() => {
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
          管理 Codex 账号 · 共 {{ totalAccounts }} 个账号
        </p>
      </div>

      <AppTopbar class="mt-0.5" :refreshing="refreshingList" @refresh="refreshAccounts" />
    </header>

    <div class="mt-6 flex shrink-0 items-center justify-between gap-4">
      <div class="flex items-center gap-3">
        <BaseInput v-model="searchQuery" placeholder="搜索邮箱或 ID..." class="w-80">
          <template #prefix>
            <Search class="size-4.5 text-(--cp-text-tertiary)" />
          </template>
        </BaseInput>

        <BaseButton
          v-if="selectedIds.size > 0"
          variant="danger"
          size="md"
          :disabled="batchDeleting"
          @click="showDeleteModal = true"
        >
          <Trash2 class="size-4" />
          删除选中 ({{ selectedIds.size }})
        </BaseButton>
      </div>

      <div class="flex items-center gap-2">
        <BaseIconButton variant="ghost" size="md" title="导出账号">
          <Download class="size-4.5" />
        </BaseIconButton>

        <BaseIconButton variant="ghost" size="md" title="导入账号">
          <Upload class="size-4.5" />
        </BaseIconButton>

        <BaseIconButton
          variant="ghost"
          size="md"
          title="刷新列表"
          :loading="refreshingList"
          :disabled="loading"
          @click="refreshAccounts"
        >
          <RefreshCw class="size-4.5" />
        </BaseIconButton>

        <BaseButton variant="primary" size="md" @click="showCreateModal = true">
          <Plus class="size-4" />
          添加账号
        </BaseButton>
      </div>
    </div>

    <BaseCard v-loading="loading" class="mt-5 flex min-h-0 flex-1 p-0">
      <BaseTable
        :columns="accountColumns"
        :rows="filteredAccounts"
        :selected-row-keys="selectedRowKeys"
        :pagination="accountPagination"
        empty-text="暂无账号数据"
        @page-change="handlePageChange"
        @page-size-change="handlePageSizeChange"
      >
        <template #header-selection>
          <BaseCheckbox
            :model-value="allSelected"
            :indeterminate="indeterminate"
            label="选择当前页账号"
            size="table"
            @update:model-value="toggleAll"
          />
        </template>

        <template #selection="{ row }">
          <BaseCheckbox
            :model-value="selectedIds.has(row.id)"
            label="选择账号"
            size="table"
            @update:model-value="toggleSelection(row.id)"
          />
        </template>

        <template #identity="{ row }">
          <span class="text-[14px] font-medium text-(--cp-text-primary)">
            {{ row.email || row.accountId || row.id }}
          </span>
        </template>

        <template #status="{ row }">
          <span
            class="inline-flex items-center rounded-full px-2 py-0.5 text-[12px] font-medium"
            :class="{
              'bg-green-50 text-green-700': statusTone(row.status) === 'success',
              'bg-red-50 text-red-700': statusTone(row.status) === 'danger',
              'bg-yellow-50 text-yellow-700': statusTone(row.status) === 'warning',
              'bg-blue-50 text-blue-700': statusTone(row.status) === 'info',
              'bg-gray-50 text-gray-700': statusTone(row.status) === 'normal',
            }"
          >
            {{ statusLabel(row.status) }}
          </span>
        </template>

        <template #planType="{ row }">
          <span class="capitalize text-(--cp-text-secondary)">
            {{ row.planType || '—' }}
          </span>
        </template>

        <template #requests>
          <span class="font-mono text-[14px] text-(--cp-text-secondary)"> — </span>
        </template>

        <template #tokens>
          <span class="font-mono text-[14px] text-(--cp-text-secondary)"> — </span>
        </template>

        <template #updatedAt="{ row }">
          <span class="text-(--cp-text-secondary)">
            {{ formatDate(row.updatedAt) }}
          </span>
        </template>

        <template #accessTokenExpiresAt="{ row }">
          <span class="text-(--cp-text-secondary)">
            {{ formatDate(row.accessTokenExpiresAt) }}
          </span>
        </template>

        <template #actions="{ row }">
          <div class="flex items-center justify-start gap-1">
            <BaseIconButton
              variant="ghost"
              size="sm"
              title="刷新令牌"
              :loading="refreshingAccountIds.has(row.id)"
              @click="handleRefresh(row.id)"
            >
              <RefreshCw class="size-3.5" />
            </BaseIconButton>
            <BaseIconButton
              variant="ghost"
              size="sm"
              title="删除账号"
              :disabled="deletingAccount"
              @click="requestDeleteAccount(row)"
            >
              <Trash2 class="size-3.5" />
            </BaseIconButton>
          </div>
        </template>
      </BaseTable>
    </BaseCard>

    <!-- 创建账号模态框 -->
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
            Refresh Token <span class="text-red-500">*</span>
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
