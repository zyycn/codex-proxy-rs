<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { Plus, RefreshCw, Trash2, Download, Upload, Search } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSpinner from '@/components/base/BaseSpinner.vue'
import AppTopbar from '@/layout/components/AppTopbar.vue'

import type { Account } from '@/api'
import { batchDeleteAccounts, createAccount, deleteAccount, getAccounts, refreshAccount, updateAccountLabel } from '@/api'

const loading = ref(true)
const accounts = ref<Account[]>([])
const searchQuery = ref('')
const selectedIds = ref<Set<string>>(new Set())
const showCreateModal = ref(false)
const showDeleteModal = ref(false)
const editingLabel = ref<{ id: string, value: string } | null>(null)

const createForm = ref({
  refreshToken: '',
  label: '',
})

const statusLabels: Record<Account['status'], string> = {
  active: '正常',
  expired: '已过期',
  disabled: '已禁用',
  banned: '已封禁',
  quota_exhausted: '配额耗尽',
  refreshing: '刷新中',
}

const statusTones: Record<Account['status'], 'success' | 'danger' | 'warning' | 'info' | 'normal'> = {
  active: 'success',
  expired: 'warning',
  disabled: 'normal',
  banned: 'danger',
  quota_exhausted: 'warning',
  refreshing: 'info',
}

const filteredAccounts = computed(() => {
  if (!searchQuery.value) return accounts.value
  const query = searchQuery.value.toLowerCase()
  return accounts.value.filter(acc =>
    (acc.email ?? '').toLowerCase().includes(query)
    || acc.label?.toLowerCase().includes(query)
    || acc.id.toLowerCase().includes(query),
  )
})

const allSelected = computed(() =>
  selectedIds.value.size === filteredAccounts.value.length && filteredAccounts.value.length > 0,
)

const indeterminate = computed(() =>
  selectedIds.value.size > 0 && selectedIds.value.size < filteredAccounts.value.length,
)

async function loadAccounts() {
  try {
    loading.value = true
    const data = await getAccounts()
    accounts.value = data
  } finally {
    loading.value = false
  }
}

async function handleCreate() {
  if (!createForm.value.refreshToken.trim()) return

  try {
    await createAccount({
      refreshToken: createForm.value.refreshToken,
      label: createForm.value.label || undefined,
    })
    showCreateModal.value = false
    createForm.value = { refreshToken: '', label: '' }
    await loadAccounts()
  } catch (error) {
    console.error('Failed to create account:', error)
  }
}

async function handleDelete(accountId: string) {
  try {
    await deleteAccount(accountId)
    await loadAccounts()
  } catch (error) {
    console.error('Failed to delete account:', error)
  }
}

async function handleBatchDelete() {
  if (selectedIds.value.size === 0) return

  try {
    await batchDeleteAccounts([...selectedIds.value])
    selectedIds.value.clear()
    showDeleteModal.value = false
    await loadAccounts()
  } catch (error) {
    console.error('Failed to batch delete accounts:', error)
  }
}

async function handleRefresh(accountId: string) {
  try {
    await refreshAccount(accountId)
    await loadAccounts()
  } catch (error) {
    console.error('Failed to refresh account:', error)
  }
}

async function handleUpdateLabel(accountId: string, label: string) {
  try {
    await updateAccountLabel(accountId, label || null)
    editingLabel.value = null
    await loadAccounts()
  } catch (error) {
    console.error('Failed to update label:', error)
  }
}

function startEditLabel(account: Account) {
  editingLabel.value = { id: account.id, value: account.label || '' }
}

function cancelEditLabel() {
  editingLabel.value = null
}

function toggleSelection(accountId: string) {
  if (selectedIds.value.has(accountId)) {
    selectedIds.value.delete(accountId)
  } else {
    selectedIds.value.add(accountId)
  }
}

function toggleAll() {
  if (selectedIds.value.size === filteredAccounts.value.length) {
    selectedIds.value.clear()
  } else {
    filteredAccounts.value.forEach(acc => selectedIds.value.add(acc.id))
  }
}

function formatDate(dateStr?: string): string {
  if (!dateStr) return '—'
  const date = new Date(dateStr)
  const now = new Date()
  const diff = now.getTime() - date.getTime()
  const minutes = Math.floor(diff / 60000)
  const hours = Math.floor(diff / 3600000)
  const days = Math.floor(diff / 86400000)

  if (minutes < 1) return '刚刚'
  if (minutes < 60) return `${minutes}分钟前`
  if (hours < 24) return `${hours}小时前`
  if (days < 7) return `${days}天前`
  return date.toLocaleDateString('zh-CN')
}

onMounted(() => {
  loadAccounts()
})
</script>

<template>
  <div class="w-full min-w-295 p-7">
    <header class="flex h-17 items-start justify-between">
      <div>
        <h1 class="mt-0 text-[34px] leading-[1.15] font-extrabold mb-0 text-(--cp-text-primary)">
          账号管理
        </h1>
        <p class="mt-2.5 text-[15px] leading-[1.15] font-semibold mb-0 text-(--cp-text-secondary)">
          管理 Codex 账号 · 共 {{ accounts.length }} 个账号
        </p>
      </div>

      <AppTopbar class="mt-0.5" />
    </header>

    <div class="mt-6 flex items-center justify-between gap-4">
      <div class="flex items-center gap-3">
        <BaseInput
          v-model="searchQuery"
          placeholder="搜索邮箱、标签或 ID..."
          class="w-80"
        >
          <template #prefix>
            <Search class="size-4.5 text-(--cp-text-tertiary)" />
          </template>
        </BaseInput>

        <BaseButton
          v-if="selectedIds.size > 0"
          variant="danger"
          size="md"
          @click="showDeleteModal = true"
        >
          <Trash2 class="size-4" />
          删除选中 ({{ selectedIds.size }})
        </BaseButton>
      </div>

      <div class="flex items-center gap-2">
        <BaseIconButton
          variant="ghost"
          size="md"
          title="导出账号"
        >
          <Download class="size-4.5" />
        </BaseIconButton>

        <BaseIconButton
          variant="ghost"
          size="md"
          title="导入账号"
        >
          <Upload class="size-4.5" />
        </BaseIconButton>

        <BaseIconButton
          variant="ghost"
          size="md"
          title="刷新列表"
          @click="loadAccounts"
        >
          <RefreshCw class="size-4.5" />
        </BaseIconButton>

        <BaseButton
          variant="primary"
          size="md"
          @click="showCreateModal = true"
        >
          <Plus class="size-4" />
          添加账号
        </BaseButton>
      </div>
    </div>

    <BaseCard class="mt-5 p-0">
      <BaseSpinner v-if="loading" class="py-20" />

      <BaseEmpty
        v-else-if="filteredAccounts.length === 0"
        message="暂无账号数据"
        class="py-20"
      />

      <BaseScrollbar v-else max-height="calc(100vh - 280px)">
        <table class="w-full border-separate border-spacing-y-2 text-left">
          <thead>
            <tr class="h-10 text-[11px] font-bold text-(--cp-text-muted)">
              <th class="w-12 px-3">
                <input
                  type="checkbox"
                  class="cursor-pointer"
                  :checked="allSelected"
                  :indeterminate="indeterminate"
                  @change="toggleAll"
                >
              </th>
              <th class="px-3">邮箱 / 标签</th>
              <th class="px-3">状态</th>
              <th class="px-3">套餐</th>
              <th class="px-3 text-right">请求数</th>
              <th class="px-3 text-right">总 Tokens</th>
              <th class="px-3">最后使用</th>
              <th class="px-3">过期时间</th>
              <th class="px-3 text-right">操作</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="account in filteredAccounts"
              :key="account.id"
              class="h-13 transition-colors"
              :class="{ 'bg-(--cp-bg-tertiary)': selectedIds.has(account.id) }"
            >
              <td class="px-3 rounded-l-lg">
                <input
                  type="checkbox"
                  class="cursor-pointer"
                  :checked="selectedIds.has(account.id)"
                  @change="toggleSelection(account.id)"
                >
              </td>
              <td class="px-3">
                <div class="flex flex-col gap-0.5">
                  <span class="text-[14px] font-medium text-(--cp-text-primary)">
                    {{ account.email }}
                  </span>
                  <div v-if="editingLabel?.id === account.id" class="flex items-center gap-2">
                    <input
                      v-model="editingLabel.value"
                      type="text"
                      class="text-[13px] px-2 py-0.5 rounded border border-(--cp-border-primary) bg-(--cp-bg-primary)"
                      @keyup.enter="handleUpdateLabel(account.id, editingLabel.value)"
                      @keyup.escape="cancelEditLabel"
                    >
                    <button
                      class="text-[12px] text-(--cp-accent-primary) hover:underline"
                      @click="handleUpdateLabel(account.id, editingLabel.value)"
                    >
                      保存
                    </button>
                    <button
                      class="text-[12px] text-(--cp-text-tertiary) hover:underline"
                      @click="cancelEditLabel"
                    >
                      取消
                    </button>
                  </div>
                  <button
                    v-else
                    class="text-[13px] text-(--cp-text-tertiary) hover:text-(--cp-accent-primary) text-left"
                    @click="startEditLabel(account)"
                  >
                    {{ account.label || '添加标签...' }}
                  </button>
                </div>
              </td>
              <td class="px-3">
                <span
                  class="inline-flex items-center px-2 py-0.5 rounded-full text-[12px] font-medium"
                  :class="{
                    'bg-green-50 text-green-700': statusTones[account.status] === 'success',
                    'bg-red-50 text-red-700': statusTones[account.status] === 'danger',
                    'bg-yellow-50 text-yellow-700': statusTones[account.status] === 'warning',
                    'bg-blue-50 text-blue-700': statusTones[account.status] === 'info',
                    'bg-gray-50 text-gray-700': statusTones[account.status] === 'normal',
                  }"
                >
                  {{ statusLabels[account.status] }}
                </span>
              </td>
              <td class="px-3">
                <span class="text-[13px] text-(--cp-text-secondary) capitalize">
                  {{ account.planType || '—' }}
                </span>
              </td>
              <td class="px-3 text-right">
                <span class="text-[14px] font-mono text-(--cp-text-secondary)">
                  —
                </span>
              </td>
              <td class="px-3 text-right">
                <span class="text-[14px] font-mono text-(--cp-text-secondary)">
                  —
                </span>
              </td>
              <td class="px-3">
                <span class="text-[13px] text-(--cp-text-secondary)">
                  {{ formatDate(account.updatedAt) }}
                </span>
              </td>
              <td class="px-3">
                <span class="text-[13px] text-(--cp-text-secondary)">
                  {{ formatDate(account.accessTokenExpiresAt) }}
                </span>
              </td>
              <td class="px-3 rounded-r-lg">
                <div class="flex items-center justify-end gap-1">
                  <BaseIconButton
                    variant="ghost"
                    size="sm"
                    title="刷新令牌"
                    @click="handleRefresh(account.id)"
                  >
                    <RefreshCw class="size-3.5" />
                  </BaseIconButton>
                  <BaseIconButton
                    variant="ghost"
                    size="sm"
                    title="删除账号"
                    @click="handleDelete(account.id)"
                  >
                    <Trash2 class="size-3.5" />
                  </BaseIconButton>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
      </BaseScrollbar>
    </BaseCard>

    <!-- 创建账号模态框 -->
    <BaseModal
      v-model="showCreateModal"
      title="添加账号"
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

        <div>
          <label class="block text-[13px] font-medium text-(--cp-text-secondary) mb-2">
            标签（可选）
          </label>
          <BaseInput
            v-model="createForm.label"
            placeholder="例如：生产账号、测试账号..."
          />
        </div>
      </div>

      <template #footer>
        <BaseButton
          variant="ghost"
          @click="showCreateModal = false"
        >
          取消
        </BaseButton>
        <BaseButton
          variant="primary"
          :disabled="!createForm.refreshToken.trim()"
          @click="handleCreate"
        >
          添加
        </BaseButton>
      </template>
    </BaseModal>

    <!-- 批量删除确认 -->
    <BaseModal
      v-model="showDeleteModal"
      title="确认删除"
      width="480px"
    >
      <p class="text-[14px] text-(--cp-text-secondary)">
        确定要删除选中的 <strong>{{ selectedIds.size }}</strong> 个账号吗？此操作不可撤销。
      </p>

      <template #footer>
        <BaseButton
          variant="ghost"
          @click="showDeleteModal = false"
        >
          取消
        </BaseButton>
        <BaseButton
          variant="danger"
          @click="handleBatchDelete"
        >
          确认删除
        </BaseButton>
      </template>
    </BaseModal>
  </div>
</template>
