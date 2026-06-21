<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { Plus, RefreshCw, Trash2, Download, Upload, Search, Copy } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSpinner from '@/components/base/BaseSpinner.vue'
import AppTopbar from '@/layout/components/AppTopbar.vue'

import type { ClientApiKey } from '@/api'
import {
  batchDeleteApiKeys,
  createApiKey,
  deleteApiKey,
  getApiKeys,
  updateApiKeyLabel,
  updateApiKeyStatus,
} from '@/api'
import { useToastStore } from '@/stores/modules/toast'

const toast = useToastStore()

const loading = ref(true)
const apiKeys = ref<ClientApiKey[]>([])
const searchQuery = ref('')
const selectedIds = ref<Set<string>>(new Set())
const showCreateModal = ref(false)
const showDeleteModal = ref(false)
const showKeyModal = ref(false)
const createdKey = ref('')
const editingLabel = ref<{ id: string, value: string } | null>(null)

const createForm = ref({
  name: '',
  label: '',
})

const filteredKeys = computed(() => {
  if (!searchQuery.value) return apiKeys.value
  const query = searchQuery.value.toLowerCase()
  return apiKeys.value.filter(key =>
    key.name.toLowerCase().includes(query)
    || key.label?.toLowerCase().includes(query)
    || key.id.toLowerCase().includes(query),
  )
})

const allSelected = computed(() =>
  selectedIds.value.size === filteredKeys.value.length && filteredKeys.value.length > 0,
)

const indeterminate = computed(() =>
  selectedIds.value.size > 0 && selectedIds.value.size < filteredKeys.value.length,
)

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

async function handleDelete(keyId: string) {
  try {
    await deleteApiKey(keyId)
    await loadApiKeys()
    toast.success('删除成功')
  } catch (error: any) {
    toast.error(error.message || '删除失败')
  }
}

async function handleBatchDelete() {
  if (selectedIds.value.size === 0) return

  try {
    await batchDeleteApiKeys([...selectedIds.value])
    selectedIds.value.clear()
    showDeleteModal.value = false
    await loadApiKeys()
    toast.success(`已删除 ${selectedIds.value.size} 个 API Key`)
  } catch (error: any) {
    toast.error(error.message || '批量删除失败')
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

function toggleSelection(keyId: string) {
  if (selectedIds.value.has(keyId)) {
    selectedIds.value.delete(keyId)
  } else {
    selectedIds.value.add(keyId)
  }
}

function toggleAll() {
  if (selectedIds.value.size === filteredKeys.value.length) {
    selectedIds.value.clear()
  } else {
    filteredKeys.value.forEach(key => selectedIds.value.add(key.id))
  }
}

function copyToClipboard(text: string) {
  navigator.clipboard.writeText(text).then(() => {
    toast.success('已复制到剪贴板')
  }).catch(() => {
    toast.error('复制失败')
  })
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

function maskKey(prefix: string): string {
  return `${prefix}••••••••••••••••`
}

onMounted(() => {
  loadApiKeys()
})
</script>

<template>
  <div class="w-full min-w-295 p-7">
    <header class="flex h-17 items-start justify-between">
      <div>
        <h1 class="mt-0 text-[34px] leading-[1.15] font-extrabold mb-0 text-(--cp-text-primary)">
          API Keys
        </h1>
        <p class="mt-2.5 text-[15px] leading-[1.15] font-semibold mb-0 text-(--cp-text-secondary)">
          管理客户端访问密钥 · 共 {{ apiKeys.length }} 个
        </p>
      </div>

      <AppTopbar class="mt-0.5" />
    </header>

    <div class="mt-6 flex items-center justify-between gap-4">
      <div class="flex items-center gap-3">
        <BaseInput
          v-model="searchQuery"
          placeholder="搜索名称、标签或 ID..."
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
          title="导出密钥"
        >
          <Download class="size-4.5" />
        </BaseIconButton>

        <BaseIconButton
          variant="ghost"
          size="md"
          title="导入密钥"
        >
          <Upload class="size-4.5" />
        </BaseIconButton>

        <BaseIconButton
          variant="ghost"
          size="md"
          title="刷新列表"
          @click="loadApiKeys"
        >
          <RefreshCw class="size-4.5" />
        </BaseIconButton>

        <BaseButton
          variant="primary"
          size="md"
          @click="showCreateModal = true"
        >
          <Plus class="size-4" />
          创建 API Key
        </BaseButton>
      </div>
    </div>

    <BaseCard class="mt-5 p-0">
      <BaseSpinner v-if="loading" class="py-20" />

      <BaseEmpty
        v-else-if="filteredKeys.length === 0"
        message="暂无 API Keys"
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
              <th class="px-3">名称 / 标签</th>
              <th class="px-3">密钥前缀</th>
              <th class="px-3">状态</th>
              <th class="px-3">创建时间</th>
              <th class="px-3">最后使用</th>
              <th class="px-3 text-right">操作</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="key in filteredKeys"
              :key="key.id"
              class="h-13 transition-colors"
              :class="{ 'bg-(--cp-bg-tertiary)': selectedIds.has(key.id) }"
            >
              <td class="px-3 rounded-l-lg">
                <input
                  type="checkbox"
                  class="cursor-pointer"
                  :checked="selectedIds.has(key.id)"
                  @change="toggleSelection(key.id)"
                >
              </td>
              <td class="px-3">
                <div class="flex flex-col gap-0.5">
                  <span class="text-[14px] font-medium text-(--cp-text-primary)">
                    {{ key.name }}
                  </span>
                  <div v-if="editingLabel?.id === key.id" class="flex items-center gap-2">
                    <input
                      v-model="editingLabel.value"
                      type="text"
                      class="text-[13px] px-2 py-0.5 rounded border border-(--cp-border-primary) bg-(--cp-bg-primary)"
                      @keyup.enter="handleUpdateLabel(key.id, editingLabel.value)"
                      @keyup.escape="cancelEditLabel"
                    >
                    <button
                      class="text-[12px] text-(--cp-accent-primary) hover:underline"
                      @click="handleUpdateLabel(key.id, editingLabel.value)"
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
                    @click="startEditLabel(key)"
                  >
                    {{ key.label || '添加标签...' }}
                  </button>
                </div>
              </td>
              <td class="px-3">
                <div class="flex items-center gap-2">
                  <code class="text-[13px] font-mono text-(--cp-text-primary)">
                    {{ maskKey(key.prefix) }}
                  </code>
                  <button
                    class="p-0 border-0 bg-transparent cursor-pointer text-(--cp-text-tertiary) hover:text-(--cp-accent-primary)"
                    @click="copyToClipboard(key.prefix)"
                  >
                    <Copy class="size-3.5" />
                  </button>
                </div>
              </td>
              <td class="px-3">
                <button
                  class="inline-flex items-center px-2 py-0.5 rounded-full text-[12px] font-medium border-0 cursor-pointer"
                  :class="{
                    'bg-green-50 text-green-700': key.enabled,
                    'bg-gray-50 text-gray-700': !key.enabled,
                  }"
                  @click="handleToggleStatus(key)"
                >
                  {{ key.enabled ? '已启用' : '已禁用' }}
                </button>
              </td>
              <td class="px-3">
                <span class="text-[13px] text-(--cp-text-secondary)">
                  {{ formatDate(key.createdAt) }}
                </span>
              </td>
              <td class="px-3">
                <span class="text-[13px] text-(--cp-text-secondary)">
                  {{ formatDate(key.lastUsedAt) }}
                </span>
              </td>
              <td class="px-3 rounded-r-lg">
                <div class="flex items-center justify-end gap-1">
                  <BaseIconButton
                    variant="ghost"
                    size="sm"
                    title="删除密钥"
                    @click="handleDelete(key.id)"
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

    <!-- 创建 API Key 模态框 -->
    <BaseModal
      v-model="showCreateModal"
      title="创建 API Key"
      width="540px"
    >
      <div class="flex flex-col gap-4">
        <div>
          <label class="block text-[13px] font-medium text-(--cp-text-secondary) mb-2">
            名称 <span class="text-red-500">*</span>
          </label>
          <BaseInput
            v-model="createForm.name"
            placeholder="例如：生产环境、测试账号..."
          />
        </div>

        <div>
          <label class="block text-[13px] font-medium text-(--cp-text-secondary) mb-2">
            标签（可选）
          </label>
          <BaseInput
            v-model="createForm.label"
            placeholder="备注信息..."
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
          :disabled="!createForm.name.trim()"
          @click="handleCreate"
        >
          创建
        </BaseButton>
      </template>
    </BaseModal>

    <!-- 显示新创建的密钥 -->
    <BaseModal
      v-model="showKeyModal"
      title="API Key 已创建"
      width="540px"
    >
      <div class="flex flex-col gap-4">
        <div class="px-4 py-3 rounded-xl bg-yellow-50 border border-yellow-200">
          <p class="m-0 text-[13px] font-medium text-yellow-800">
            ⚠️ 请妥善保存此密钥，它只会显示一次！
          </p>
        </div>

        <div>
          <label class="block text-[13px] font-medium text-(--cp-text-secondary) mb-2">
            API Key
          </label>
          <div class="flex items-center gap-2">
            <code class="flex-1 px-3 py-2.5 rounded-lg bg-(--cp-bg-subtle) text-[13px] font-mono text-(--cp-text-primary) break-all">
              {{ createdKey }}
            </code>
            <BaseIconButton
              size="md"
              title="复制"
              @click="copyToClipboard(createdKey)"
            >
              <Copy class="size-4" />
            </BaseIconButton>
          </div>
        </div>
      </div>

      <template #footer>
        <BaseButton
          variant="primary"
          @click="showKeyModal = false"
        >
          我已保存
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
        确定要删除选中的 <strong>{{ selectedIds.size }}</strong> 个 API Key 吗？此操作不可撤销。
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
