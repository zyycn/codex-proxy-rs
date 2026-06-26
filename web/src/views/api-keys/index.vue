<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { Plus, Trash2, Search, Copy } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseTable from '@/components/base/BaseTable.vue'

import { createApiKey, deleteApiKeys, getApiKeys, updateApiKey } from '@/api'
import { toast } from '@/components/base/BaseToast'

const loading = ref(true)
const apiKeys = ref<any[]>([])
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
const pendingDeleteKey = ref<any | null>(null)
const deletingKey = ref(false)
const batchDeleting = ref(false)

const createForm = ref({
  name: '',
  label: '',
})

const apiKeyColumns = [
  { key: 'selection', label: '', width: '48px', align: 'center' as const },
  { key: 'identity', label: '名称 / 标签', minWidth: '280px', flex: 1.25 },
  { key: 'prefix', label: '密钥前缀', minWidth: '300px', flex: 1.35 },
  { key: 'enabled', label: '状态', width: '112px', align: 'center' as const },
  {
    key: 'createdAtDisplay',
    label: '创建时间',
    width: '176px',
    mono: true,
    tabular: true,
    cellClass: 'text-(--cp-text-secondary)',
  },
  {
    key: 'lastUsedAtDisplay',
    label: '最后使用',
    width: '176px',
    mono: true,
    tabular: true,
    cellClass: 'text-(--cp-text-secondary)',
  },
  {
    key: 'actions',
    label: '操作',
    width: '108px',
    align: 'center' as const,
    headerClass: '!px-4',
    cellClass: '!px-4',
  },
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
    apiKeys.value = data.items
  } catch (error: any) {
    toast.error(error.message || '加载失败')
  } finally {
    loading.value = false
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

function requestDeleteKey(key: any) {
  pendingDeleteKey.value = key
  showSingleDeleteModal.value = true
}

async function handleDelete() {
  const keyId = pendingDeleteKey.value?.id
  if (!keyId) return

  try {
    deletingKey.value = true
    await deleteApiKeys({ ids: [keyId] })
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
    await deleteApiKeys({ ids: [...selectedIds.value] })
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

async function handleToggleStatus(key: any) {
  try {
    await updateApiKey({ id: key.id, status: key.enabled ? 'disabled' : 'active' })
    await loadApiKeys()
    toast.success(key.enabled ? '已禁用' : '已启用')
  } catch (error: any) {
    toast.error(error.message || '状态更新失败')
  }
}

async function handleUpdateLabel(keyId: string, label: string) {
  try {
    await updateApiKey({ id: keyId, label: label || null })
    editingLabel.value = null
    await loadApiKeys()
    toast.success('标签已更新')
  } catch (error: any) {
    toast.error(error.message || '标签更新失败')
  }
}

function startEditLabel(key: any) {
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
        <h1 class="mt-0 mb-0 text-[34px] leading-[1.15] font-extrabold text-(--cp-text-primary)">
          API 密钥
        </h1>
        <p class="mt-2.5 mb-0 text-[15px] leading-[1.15] font-semibold text-(--cp-text-secondary)">
          签发与维护客户端访问凭证，控制网关调用入口。
        </p>
      </div>
    </header>

    <BaseCard
      v-loading="loading"
      :padded="false"
      class="mt-5 flex min-h-0 flex-1 flex-col"
      header-class="px-5 pt-4"
      body-class="min-h-0 flex-1 px-5 pt-3"
    >
      <template #header>
        <div class="flex flex-wrap items-center justify-between gap-3" aria-label="API Key 筛选">
          <div class="flex min-w-0 flex-1 flex-wrap items-center gap-3">
            <BaseInput
              v-model="searchQuery"
              placeholder="搜索名称、标签或 ID"
              class="min-w-64 flex-1 sm:max-w-96"
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
              <template #icon>
                <Trash2 class="size-4" />
              </template>
              删除选中 ({{ selectedIds.size }})
            </BaseButton>
          </div>

          <div class="flex shrink-0 items-center gap-2">
            <BaseButton variant="primary" @click="showCreateModal = true">
              <template #icon>
                <Plus class="size-4" />
              </template>
              创建 API Key
            </BaseButton>
          </div>
        </div>
      </template>

      <template #body>
        <BaseTable
          :columns="apiKeyColumns"
          :rows="pagedKeys"
          :selected-row-keys="selectedRowKeys"
          :pagination="apiKeyPagination"
          empty-text="暂无 API Key"
          min-width="1240px"
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
              <span class="text-[13px] font-bold text-(--cp-text-primary)">
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
                class="text-left text-[12px] font-[650] text-(--cp-text-tertiary) hover:text-(--cp-info-text)"
                @click="startEditLabel(row)"
              >
                {{ row.label }}
              </button>
            </div>
          </template>

          <template #prefix="{ row }">
            <div class="flex items-center gap-2">
              <code
                class="block min-w-0 truncate font-mono text-[12px] font-[650] text-(--cp-text-primary)"
              >
                {{ maskKey(row.prefix) }}
              </code>
              <BaseButton
                icon-only
                variant="ghost"
                size="sm"
                label="复制密钥前缀"
                @click="copyToClipboard(row.prefix)"
              >
                <Copy class="size-3.5" />
              </BaseButton>
            </div>
          </template>

          <template #enabled="{ row }">
            <button
              class="inline-flex h-6 min-w-14 cursor-pointer items-center justify-center rounded-full border-0 px-2 text-[12px] leading-none font-bold"
              :class="{
                'bg-(--cp-success-bg) text-(--cp-success-text)': row.enabled,
                'bg-(--cp-bg-subtle) text-(--cp-text-secondary)': !row.enabled,
              }"
              @click="handleToggleStatus(row)"
            >
              {{ row.enabled ? '已启用' : '已禁用' }}
            </button>
          </template>

          <template #actions="{ row }">
            <div class="flex items-center justify-center">
              <BaseButton
                icon-only
                variant="ghost"
                size="sm"
                label="删除密钥"
                :disabled="deletingKey"
                @click="requestDeleteKey(row)"
              >
                <Trash2 class="size-3.5" />
              </BaseButton>
            </div>
          </template>
        </BaseTable>
      </template>
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
            <BaseButton icon-only size="md" title="复制" @click="copyToClipboard(createdKey)">
              <Copy class="size-4" />
            </BaseButton>
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
