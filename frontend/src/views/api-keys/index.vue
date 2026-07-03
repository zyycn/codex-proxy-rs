<script setup lang="ts">
import { ref } from 'vue'
import { Trash2 } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { apiKeyColumns } from './constants'
import { useApiKeyFilters } from './composables/useApiKeyFilters'
import { useApiKeyMutations } from './composables/useApiKeyMutations'
import { useApiKeysTable } from './composables/useApiKeysTable'
import ApiKeyCreateModal from './components/ApiKeyCreateModal.vue'
import ApiKeyFilters from './components/ApiKeyFilters.vue'
import ApiKeyIdentityCell from './components/ApiKeyIdentityCell.vue'
import ApiKeyPrefixCell from './components/ApiKeyPrefixCell.vue'
import ApiKeyStatusToggle from './components/ApiKeyStatusToggle.vue'

const selectedIds = ref<Set<string>>(new Set())
const {
  loading,
  apiKeys,
  showCreateModal,
  showDeleteModal,
  showSingleDeleteModal,
  showKeyModal,
  createdKey,
  editingLabel,
  pendingDeleteKey,
  creatingKey,
  deletingKey,
  batchDeleting,
  updatingStatusKeyIds,
  savingLabelKeyIds,
  createForm,
  handleCreate,
  requestDeleteKey,
  handleDelete,
  handleBatchDelete,
  handleToggleStatus,
  startEditLabel,
  cancelEditLabel,
  currentEditingLabelValue,
  submitEditingLabel,
  updateEditingLabelValue,
  copyToClipboard,
  maskKey,
} = useApiKeyMutations(selectedIds)

const { searchQuery, pagedKeys, apiKeyPagination, handlePageChange, handlePageSizeChange } =
  useApiKeyFilters(apiKeys)

const { allSelected, indeterminate, selectedRowKeys, toggleSelection, toggleAll } = useApiKeysTable(
  pagedKeys,
  selectedIds,
)
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
      :padded="false"
      class="mt-5 flex h-[calc(100vh-136px)] min-h-125 flex-col"
      header-class="px-5 pt-4"
      body-class="flex min-h-0 flex-1 px-5 py-3"
    >
      <template #header>
        <ApiKeyFilters
          v-model:search="searchQuery"
          :batch-deleting="batchDeleting"
          :selected-count="selectedIds.size"
          @create="showCreateModal = true"
          @delete-selected="showDeleteModal = true"
        />
      </template>

      <template #body>
        <BaseTable
          class="min-h-0 flex-1"
          :columns="apiKeyColumns"
          :rows="pagedKeys"
          :loading="loading"
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
            <ApiKeyIdentityCell
              :api-key="row"
              :editing="editingLabel?.id === row.id"
              :editing-value="currentEditingLabelValue()"
              :saving="savingLabelKeyIds.has(row.id)"
              @cancel-edit="cancelEditLabel"
              @start-edit="startEditLabel"
              @submit-edit="submitEditingLabel"
              @update-edit="updateEditingLabelValue"
            />
          </template>

          <template #prefix="{ row }">
            <ApiKeyPrefixCell
              :key-value="row.key"
              :masked-prefix="maskKey(row.prefix)"
              @copy="copyToClipboard"
            />
          </template>

          <template #enabled="{ row }">
            <ApiKeyStatusToggle
              :api-key="row"
              :loading="updatingStatusKeyIds.has(row.id)"
              @toggle="handleToggleStatus"
            />
          </template>

          <template #actions="{ row }">
            <div class="flex items-center justify-start">
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

    <ApiKeyCreateModal
      v-model="showCreateModal"
      v-model:created-open="showKeyModal"
      v-model:form="createForm"
      :created-key="createdKey"
      :saving="creatingKey"
      @copy="copyToClipboard"
      @create="handleCreate"
    />

    <BaseConfirmModal
      v-model="showDeleteModal"
      title="确认删除"
      description="删除后这些 API Key 将立即失效，此操作不可撤销。"
      variant="danger"
      confirm-text="确认删除"
      :loading="batchDeleting"
      width="480px"
      @confirm="handleBatchDelete"
    >
      <p class="m-0">确定要删除选中的 {{ selectedIds.size }} 个 API Key 吗？此操作不可撤销。</p>
    </BaseConfirmModal>

    <BaseConfirmModal
      v-model="showSingleDeleteModal"
      title="删除 API Key"
      description="删除后该 API Key 将立即失效，此操作不可撤销。"
      variant="danger"
      confirm-text="确认删除"
      :loading="deletingKey"
      width="480px"
      @confirm="handleDelete"
    >
      <p class="m-0">
        确定要删除 {{ pendingDeleteKey?.name || pendingDeleteKey?.prefix || '该 API Key' }} 吗？
      </p>
    </BaseConfirmModal>
  </div>
</template>
