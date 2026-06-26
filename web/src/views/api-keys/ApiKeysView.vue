<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { Plus, RefreshCw, Trash2, Download, Upload, Search, Copy } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseTable from '@/components/base/BaseTable.vue'
import { withMinimumDuration } from '@/utils/async'

import type { ClientApiKey } from '@/api'
import {
  batchDeleteApiKeys,
  createApiKey,
  deleteApiKey,
  getApiKeys,
  updateApiKeyLabel,
  updateApiKeyStatus,
} from '@/api'
import { toast } from '@/components/base/BaseToast'

const loading = ref(true)
const apiKeys = ref<ClientApiKey[]>([])
const page = ref(1)
const pageSize = ref(20)
const searchQuery = ref('')
const selectedIds = ref<Set<string>>(new Set())
const showCreateModal = ref(false)
const showDeleteModal = ref(false)
const showSingleDeleteModal = ref(false)
const showKeyModal = ref(false)
const createdKey = ref('')
const editingLabel = ref<{ id: string; value: string } | null>(null)
const pendingDeleteKey = ref<ClientApiKey | null>(null)
const refreshingList = ref(false)
const deletingKey = ref(false)
const batchDeleting = ref(false)

const createForm = ref({
  name: '',
  label: '',
})

const apiKeyColumns = [
  { key: 'selection', label: '', width: '48px', align: 'center' as const },
  { key: 'identity', label: '名称 / 标签' },
  { key: 'prefix', label: '密钥前缀' },
  { key: 'enabled', label: '状态' },
  { key: 'createdAt', label: '创建时间' },
  { key: 'lastUsedAt', label: '最后使用' },
  { key: 'actions', label: '操作', width: '80px', align: 'right' as const },
]

const filteredKeys = computed(() => {
  if (!searchQuery.value) return apiKeys.value
  const query = searchQuery.value.toLowerCase()
  return apiKeys.value.filter(
    (key) =>
      key.name.toLowerCase().includes(query) ||
      key.label?.toLowerCase().includes(query) ||
      key.id.toLowerCase().includes(query),
  )
})

const pagedKeys = computed(() => {
  const start = (page.value - 1) * pageSize.value
  return filteredKeys.value.slice(start, start + pageSize.value)
})

const allSelected = computed(
  () => pagedKeys.value.length > 0 && pagedKeys.value.every((key) => selectedIds.value.has(key.id)),
)

const indeterminate = computed(
  () => pagedKeys.value.some((key) => selectedIds.value.has(key.id)) && !allSelected.value,
)

const selectedRowKeys = computed(() => [...selectedIds.value])
const apiKeyPagination = computed(() => ({
  page: page.value,
  pageSize: pageSize.value,
  total: filteredKeys.value.length,
  pageSizes: [10, 20, 50, 100],
}))

async function loadApiKeys() {
  try {
    loading.value = true
    const data = await getApiKeys()
    apiKeys.value = data
  } catch (error: any) {
    toast.error(error.message || '加载失败')
  } finally {
    loading.value = false
  }
}

async function refreshApiKeys() {
  if (refreshingList.value || loading.value) return
  refreshingList.value = true
  try {
    await withMinimumDuration(loadApiKeys)
  } finally {
    refreshingList.value = false
  }
}

async function handleCreate() {
  if (!createForm.value.name.trim()) {
    toast.warning('请输入 API Key 名称')
    return
  }

  try {
    const result = await createApiKey({
      name: createForm.value.name,
      label: createForm.value.label || undefined,
    })

    createdKey.value = result.key
    showCreateModal.value = false
    showKeyModal.value = true
    createForm.value = { name: '', label: '' }

    await loadApiKeys()
    toast.success('API Key 创建成功')
  } catch (error: any) {
    toast.error(error.message || '创建失败')
  }
}

function requestDeleteKey(key: ClientApiKey) {
  pendingDeleteKey.value = key
  showSingleDeleteModal.value = true
}

async function handleDelete() {
  const keyId = pendingDeleteKey.value?.id
  if (!keyId) return

  try {
    deletingKey.value = true
    await deleteApiKey(keyId)
    showSingleDeleteModal.value = false
    pendingDeleteKey.value = null
    await loadApiKeys()
    toast.success('删除成功')
  } catch (error: any) {
    toast.error(error.message || '删除失败')
  } finally {
    deletingKey.value = false
  }
}

async function handleBatchDelete() {
  if (selectedIds.value.size === 0) return

  try {
    batchDeleting.value = true
    const deleteCount = selectedIds.value.size
    await batchDeleteApiKeys([...selectedIds.value])
    selectedIds.value = new Set()
    showDeleteModal.value = false
    await loadApiKeys()
    toast.success(`已删除 ${deleteCount} 个 API Key`)
  } catch (error: any) {
    toast.error(error.message || '批量删除失败')
  } finally {
    batchDeleting.value = false
  }
}

async function handleToggleStatus(key: ClientApiKey) {
  try {
    await updateApiKeyStatus(key.id, !key.enabled)
    await loadApiKeys()
    toast.success(key.enabled ? '已禁用' : '已启用')
  } catch (error: any) {
    toast.error(error.message || '状态更新失败')
  }
}

async function handleUpdateLabel(keyId: string, label: string) {
  try {
    await updateApiKeyLabel(keyId, label || null)
    editingLabel.value = null
    await loadApiKeys()
    toast.success('标签已更新')
  } catch (error: any) {
    toast.error(error.message || '标签更新失败')
  }
}

function startEditLabel(key: ClientApiKey) {
  editingLabel.value = { id: key.id, value: key.label || '' }
}

function cancelEditLabel() {
  editingLabel.value = null
}

function currentEditingLabelValue() {
  return editingLabel.value?.value ?? ''
}

function submitEditingLabel(keyId: string) {
  void handleUpdateLabel(keyId, currentEditingLabelValue())
}

function updateEditingLabelValue(event: Event) {
  if (!editingLabel.value) {
    return
  }

  editingLabel.value.value = (event.target as HTMLInputElement).value
}

function toggleSelection(keyId: string) {
  if (selectedIds.value.has(keyId)) {
    selectedIds.value.delete(keyId)
  } else {
    selectedIds.value.add(keyId)
  }
}

function toggleAll() {
  if (allSelected.value) {
    pagedKeys.value.forEach((key) => selectedIds.value.delete(key.id))
  } else {
    pagedKeys.value.forEach((key) => selectedIds.value.add(key.id))
  }
}

function handlePageChange(nextPage: number) {
  page.value = nextPage
}

function handlePageSizeChange(nextPageSize: number) {
  pageSize.value = nextPageSize
  page.value = 1
}

function copyToClipboard(text: string) {
  navigator.clipboard
    .writeText(text)
    .then(() => {
      toast.success('已复制到剪贴板')
    })
    .catch(() => {
      toast.error('复制失败')
    })
}

function maskKey(prefix: string): string {
  return `${prefix}••••••••••••••••`
}

onMounted(() => {
  loadApiKeys()
})

watch(searchQuery, () => {
  page.value = 1
})

watch(filteredKeys, () => {
  const totalPages = Math.max(1, Math.ceil(filteredKeys.value.length / pageSize.value))
  if (page.value > totalPages) {
    page.value = totalPages
  }
})
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col overflow-hidden">
    <header class="flex h-17 shrink-0 items-start justify-between">
      <div>
        <h1 class="mt-0 text-[34px] leading-[1.15] font-extrabold mb-0 text-(--cp-text-primary)">
          API Keys
        </h1>
        <p class="mt-2.5 text-[15px] leading-[1.15] font-semibold mb-0 text-(--cp-text-secondary)">
          管理客户端访问密钥 · 共 {{ apiKeys.length }} 个
        </p>
      </div>
    </header>

    <div class="mt-6 flex shrink-0 items-center justify-between gap-4">
      <div class="flex items-center gap-3">
        <BaseInput v-model="searchQuery" placeholder="搜索名称、标签或 ID..." class="w-80">
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

      <div class="flex items-center gap-2">
        <BaseIconButton variant="ghost" size="md" title="导出密钥">
          <Download class="size-4.5" />
        </BaseIconButton>

        <BaseIconButton variant="ghost" size="md" title="导入密钥">
          <Upload class="size-4.5" />
        </BaseIconButton>

        <BaseIconButton
          variant="ghost"
          size="md"
          title="刷新列表"
          :loading="refreshingList"
          :disabled="loading"
          @click="refreshApiKeys"
        >
          <RefreshCw class="size-4.5" />
        </BaseIconButton>

        <BaseButton variant="primary" @click="showCreateModal = true">
          <Plus class="size-4" />
          创建 API Key
        </BaseButton>
      </div>
    </div>

    <BaseCard v-loading="loading" class="mt-5 flex min-h-0 flex-1 p-0">
      <BaseTable
        :columns="apiKeyColumns"
        :rows="pagedKeys"
        :selected-row-keys="selectedRowKeys"
        :pagination="apiKeyPagination"
        empty-text="暂无 API Keys"
        @page-change="handlePageChange"
        @page-size-change="handlePageSizeChange"
      >
        <template #header-selection>
          <BaseCheckbox
            :model-value="allSelected"
            :indeterminate="indeterminate"
            label="选择当前页密钥"
            @update:model-value="toggleAll"
          />
        </template>

        <template #selection="{ row }">
          <BaseCheckbox
            :model-value="selectedIds.has(row.id)"
            label="选择密钥"
            @update:model-value="toggleSelection(row.id)"
          />
        </template>

        <template #identity="{ row }">
          <div class="flex flex-col gap-0.5">
            <span class="text-[14px] font-medium text-(--cp-text-primary)">
              {{ row.name }}
            </span>
            <div v-if="editingLabel?.id === row.id" class="flex items-center gap-2">
              <input
                :value="currentEditingLabelValue()"
                type="text"
                class="h-(--cp-input-height-inline) min-w-34 rounded-(--cp-input-radius-small) border-0 bg-(--cp-input-soft-bg) px-2.5 text-[13px] font-[650] text-(--cp-text-primary) shadow-(--cp-shadow-input) outline-none transition placeholder:text-(--cp-text-muted) focus:bg-(--cp-input-soft-bg-focus) focus:shadow-(--cp-shadow-input-focus)"
                @input="updateEditingLabelValue"
                @keyup.enter="submitEditingLabel(row.id)"
                @keyup.escape="cancelEditLabel"
              />
              <button
                class="text-[12px] text-(--cp-accent-primary) hover:underline"
                @click="submitEditingLabel(row.id)"
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
              v-else-if="row.label"
              class="text-left text-[13px] text-(--cp-text-tertiary) hover:text-(--cp-accent-primary)"
              @click="startEditLabel(row)"
            >
              {{ row.label }}
            </button>
          </div>
        </template>

        <template #prefix="{ row }">
          <div class="flex items-center gap-2">
            <code class="font-mono text-(--cp-text-primary)">
              {{ maskKey(row.prefix) }}
            </code>
            <button
              class="cursor-pointer border-0 bg-transparent p-0 text-(--cp-text-tertiary) hover:text-(--cp-accent-primary)"
              @click="copyToClipboard(row.prefix)"
            >
              <Copy class="size-3.5" />
            </button>
          </div>
        </template>

        <template #enabled="{ row }">
          <button
            class="inline-flex cursor-pointer items-center rounded-full border-0 px-2 py-0.5 text-[12px] font-medium"
            :class="{
              'bg-(--cp-success-bg) text-(--cp-success-text)': row.enabled,
              'bg-(--cp-bg-subtle) text-(--cp-text-secondary)': !row.enabled,
            }"
            @click="handleToggleStatus(row)"
          >
            {{ row.enabled ? '已启用' : '已禁用' }}
          </button>
        </template>

        <template #createdAt="{ row }">
          <span class="text-(--cp-text-secondary)">
            {{ row.createdAtDisplay }}
          </span>
        </template>

        <template #lastUsedAt="{ row }">
          <span class="text-(--cp-text-secondary)">
            {{ row.lastUsedAtDisplay }}
          </span>
        </template>

        <template #actions="{ row }">
          <div class="flex items-center justify-end gap-1">
            <BaseIconButton
              variant="ghost"
              size="sm"
              title="删除密钥"
              :disabled="deletingKey"
              @click="requestDeleteKey(row)"
            >
              <Trash2 class="size-3.5" />
            </BaseIconButton>
          </div>
        </template>
      </BaseTable>
    </BaseCard>

    <!-- 创建 API Key 模态框 -->
    <BaseModal
      v-model="showCreateModal"
      title="创建 API Key"
      description="为当前代理管理端生成一个新的访问密钥。创建后请立即保存。"
      variant="info"
      width="540px"
    >
      <div class="flex flex-col gap-4">
        <div>
          <label class="block text-[13px] font-medium text-(--cp-text-secondary) mb-2">
            名称 <span class="text-(--cp-danger)">*</span>
          </label>
          <BaseInput v-model="createForm.name" placeholder="例如：生产环境、测试账号..." />
        </div>

        <div>
          <label class="block text-[13px] font-medium text-(--cp-text-secondary) mb-2">
            标签（可选）
          </label>
          <BaseInput v-model="createForm.label" placeholder="备注信息..." />
        </div>
      </div>

      <template #footer>
        <BaseButton variant="ghost" @click="showCreateModal = false"> 取消 </BaseButton>
        <BaseButton variant="primary" :disabled="!createForm.name.trim()" @click="handleCreate">
          创建
        </BaseButton>
      </template>
    </BaseModal>

    <!-- 显示新创建的密钥 -->
    <BaseModal
      v-model="showKeyModal"
      title="API Key 已创建"
      description="密钥只会显示一次，关闭弹窗后无法再次查看完整内容。"
      variant="success"
      width="540px"
    >
      <div class="flex flex-col gap-4">
        <div
          class="rounded-(--cp-input-radius-base) border border-(--cp-warning-border) bg-(--cp-warning-bg) px-4 py-3"
        >
          <p class="m-0 text-[13px] font-semibold text-(--cp-warning-text)">
            请妥善保存此密钥，它只会显示一次。
          </p>
        </div>

        <div>
          <label class="block text-[13px] font-medium text-(--cp-text-secondary) mb-2">
            API Key
          </label>
          <div class="flex items-center gap-2">
            <code
              class="flex-1 px-3 py-2.5 rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) text-[13px] font-mono text-(--cp-text-primary) break-all"
            >
              {{ createdKey }}
            </code>
            <BaseIconButton size="md" title="复制" @click="copyToClipboard(createdKey)">
              <Copy class="size-4" />
            </BaseIconButton>
          </div>
        </div>
      </div>

      <template #footer>
        <BaseButton variant="primary" @click="showKeyModal = false"> 我已保存 </BaseButton>
      </template>
    </BaseModal>

    <BaseConfirmModal
      v-model="showDeleteModal"
      title="确认删除"
      description="删除后这些 API Key 将立即失效，此操作不可撤销。"
      :message="`确定要删除选中的 ${selectedIds.size} 个 API Key 吗？此操作不可撤销。`"
      variant="danger"
      confirm-text="确认删除"
      :loading="batchDeleting"
      width="480px"
      @confirm="handleBatchDelete"
    />

    <BaseConfirmModal
      v-model="showSingleDeleteModal"
      title="删除 API Key"
      description="删除后该 API Key 将立即失效，此操作不可撤销。"
      :message="`确定要删除 ${pendingDeleteKey?.name || pendingDeleteKey?.prefix || '该 API Key'} 吗？`"
      variant="danger"
      confirm-text="确认删除"
      :loading="deletingKey"
      width="480px"
      @confirm="handleDelete"
    />
  </div>
</template>
