<script setup lang="ts">
import type { ApiKey } from '@/api'
import { BaseCard, BaseCheckbox, BaseConfirmModal, BasePageHeader, BaseTable, BaseTablePagination } from '@codex-proxy/ui'

import { ref, shallowRef, watch } from 'vue'
import ApiKeyConfigModal from '@/components/ApiKeyConfigModal.vue'
import LastUsedAtCell from '@/components/LastUsedAtCell.vue'
import { useAccountGroupCatalog } from '@/composables/useAccountGroupCatalog'
import { usePageSelection } from '@/composables/usePageSelection'
import ApiKeyActions from './components/ApiKeyActions.vue'
import ApiKeyBudgetCell from './components/ApiKeyBudgetCell.vue'
import ApiKeyBudgetResetModal from './components/ApiKeyBudgetResetModal.vue'
import ApiKeyCreatedModal from './components/ApiKeyCreatedModal.vue'
import ApiKeyFilters from './components/ApiKeyFilters.vue'
import ApiKeyFormModal from './components/ApiKeyFormModal.vue'
import ApiKeyIdentityCell from './components/ApiKeyIdentityCell.vue'
import ApiKeyPrefixCell from './components/ApiKeyPrefixCell.vue'
import ApiKeyScopeCell from './components/ApiKeyScopeCell.vue'
import ApiKeyStatusBadge from './components/ApiKeyStatusBadge.vue'
import { useApiKeyEditor } from './composables/useApiKeyEditor'
import { useApiKeyMutations } from './composables/useApiKeyMutations'
import { useApiKeysQuery } from './composables/useApiKeysQuery'
import { useApiKeyUse } from './composables/useApiKeyUse'
import { apiKeyColumns } from './constants'

const selectedIds = ref<Set<string>>(new Set())
const showBudgetResetModal = shallowRef(false)
const resettingKey = shallowRef<ApiKey | null>(null)

function openBudgetReset(key: ApiKey) {
  resettingKey.value = key
  showBudgetResetModal.value = true
}
const {
  loading,
  apiKeys,
  loadApiKeys,
  searchQuery,
  sort,
  apiKeyPagination,
  handlePageChange,
  handlePageSizeChange,
  handleSortChange,
} = useApiKeysQuery()

const {
  groups,
  loading: loadingGroups,
  loadGroups,
} = useAccountGroupCatalog({ immediate: false })

const {
  showUseKeyModal,
  selectedUseKey,
  showCreatedKeyModal,
  createdKey,
  revealingKeyIds,
  openAiBaseUrl,
  showCreatedKey,
  clearCreatedKey,
  copyToClipboard,
  copyApiKey,
  importCreatedKeyToCcs,
  openUseKeyModal,
  importToCcs,
} = useApiKeyUse()

const {
  showFormModal,
  showAllAccountsConfirm,
  editingKey,
  savingKey,
  form,
  openCreate,
  openEdit,
  clearCustomKey,
  requestSave,
  confirmAllAccountsScope,
} = useApiKeyEditor({ reload: loadApiKeys, onCreated: showCreatedKey })

const {
  showDeleteModal,
  showSingleDeleteModal,
  pendingDeleteKey,
  deleteCount,
  deletingKey,
  batchDeleting,
  updatingStatusKeyIds,
  requestDeleteKey,
  handleDelete,
  handleBatchDelete,
  handleToggleStatus,
} = useApiKeyMutations({ selectedIds, reload: loadApiKeys })

const { allSelected, indeterminate, selectedRowKeys, toggleSelection, toggleAll } = usePageSelection(
  apiKeys,
  selectedIds,
)

watch(
  showFormModal,
  open => open && void loadGroups(),
)
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col overflow-hidden">
    <BasePageHeader
      class="h-17"
      title="API 密钥"
      description="创建和管理 API 密钥，并设置每个密钥可以使用的账号"
    />

    <BaseCard
      class="mt-5 flex h-[calc(100dvh-136px)] min-h-125 flex-col"
    >
      <template #header>
        <ApiKeyFilters
          v-model:search="searchQuery"
          :batch-deleting="batchDeleting"
          :selected-count="selectedIds.size"
          @create="openCreate"
          @delete-selected="showDeleteModal = true"
        />
      </template>

      <template #body>
        <div class="flex h-full min-h-0 flex-col">
          <BaseTable
            class="min-h-0 flex-1"
            :columns="apiKeyColumns"
            :rows="apiKeys"
            :loading="loading"
            :selected-row-keys="selectedRowKeys"
            :sort="sort"
            empty-text="暂无 API Key"
            @sort-change="handleSortChange"
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
              <ApiKeyIdentityCell :api-key="row" />
            </template>
            <template #prefix="{ row }">
              <ApiKeyPrefixCell
                :prefix="row.prefix"
                :revealing="revealingKeyIds.has(row.id)"
                @copy="copyApiKey(row)"
              />
            </template>
            <template #scope="{ row }">
              <ApiKeyScopeCell :api-key="row" />
            </template>
            <template #budget="{ row }">
              <ApiKeyBudgetCell :api-key="row" />
            </template>
            <template #limits="{ row }">
              <dl class="m-0 grid grid-cols-[auto_minmax(0,1fr)] gap-x-2 gap-y-1 text-xs tabular-nums">
                <dt class="text-right text-cp-text-tertiary">
                  并发
                </dt>
                <dd class="m-0 truncate text-cp-text" :title="String(row.maxConcurrency || '∞')">
                  {{ row.maxConcurrency || '∞' }}
                </dd>
                <dt class="text-right text-cp-text-tertiary">
                  RPM
                </dt>
                <dd class="m-0 truncate text-cp-text" :title="String(row.requestsPerMinute || '∞')">
                  {{ row.requestsPerMinute || '∞' }}
                </dd>
              </dl>
            </template>
            <template #enabled="{ row }">
              <ApiKeyStatusBadge :api-key="row" />
            </template>
            <template #lastUsedAt="{ row }">
              <LastUsedAtCell :value="row.lastUsedAt" :display="row.lastUsedAtDisplay" :full-display="row.lastUsedAtFullDisplay" />
            </template>
            <template #actions="{ row }">
              <ApiKeyActions
                :api-key="row"
                :deleting="deletingKey"
                :revealing="revealingKeyIds.has(row.id)"
                :updating-status="updatingStatusKeyIds.has(row.id)"
                @edit="openEdit"
                @reset-budget="openBudgetReset"
                @delete="requestDeleteKey"
                @import-ccs="importToCcs"
                @toggle="handleToggleStatus"
                @use="openUseKeyModal"
              />
            </template>
          </BaseTable>
          <BaseTablePagination
            :pagination="apiKeyPagination"
            :loading="loading"
            @page-change="handlePageChange"
            @page-size-change="handlePageSizeChange"
          />
        </div>
      </template>
    </BaseCard>

    <ApiKeyFormModal
      v-model="showFormModal"
      v-model:form="form"
      :groups="groups"
      :group-loading="loadingGroups"
      :editing="Boolean(editingKey)"
      :saving="savingKey"
      @save="requestSave"
      @after-leave="clearCustomKey"
    />

    <ApiKeyCreatedModal
      v-model="showCreatedKeyModal"
      :created-key="createdKey"
      @copy="copyToClipboard"
      @import-ccs="importCreatedKeyToCcs"
      @after-leave="clearCreatedKey"
    />

    <ApiKeyBudgetResetModal
      v-model="showBudgetResetModal"
      :api-key="resettingKey"
      @reset="loadApiKeys"
    />

    <ApiKeyConfigModal
      v-model="showUseKeyModal"
      :api-key="selectedUseKey"
      :api-base-url="openAiBaseUrl"
      @copy="copyToClipboard"
      @after-leave="selectedUseKey = null"
    />

    <BaseConfirmModal
      v-model="showAllAccountsConfirm"
      title="授予全部账号权限"
      description="保存后，该密钥可以使用所有账号"
      confirm-text="确认授予全部账号"
      :loading="savingKey"
      @confirm="confirmAllAccountsScope"
    >
      <p class="m-0">
        该密钥可以使用所有账号，包括以后新增和未分组的账号
      </p>
    </BaseConfirmModal>

    <BaseConfirmModal
      v-model="showDeleteModal"
      title="确认删除"
      description="删除后这些 API Key 将立即失效，此操作不可撤销"
      destructive
      confirm-text="确认删除"
      :loading="batchDeleting"
      @confirm="handleBatchDelete"
    >
      <p class="m-0">
        确定删除选中的 {{ deleteCount }} 个 API Key 吗？
      </p>
    </BaseConfirmModal>

    <BaseConfirmModal
      v-model="showSingleDeleteModal"
      title="删除 API Key"
      description="删除后该 API Key 将立即失效，此操作不可撤销"
      destructive
      confirm-text="确认删除"
      :loading="deletingKey"
      @confirm="handleDelete"
    >
      <p class="m-0">
        确定删除 {{ pendingDeleteKey?.name || pendingDeleteKey?.prefix || '该 API Key' }} 吗？
      </p>
    </BaseConfirmModal>
  </div>
</template>
