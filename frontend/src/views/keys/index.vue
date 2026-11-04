<script setup lang="ts">
import type { ApiKey } from '@/api'

import { ZCard, ZCheckbox, ZConfirmDialog, ZPageHeader, ZPagination, ZTable } from '@codex-proxy/ui'
import { ref, shallowRef, watch } from 'vue'

import { useI18n } from 'vue-i18n'
import ApiKeyConfigModal from '@/components/ApiKeyConfigModal.vue'
import ClientImportModal from '@/components/ClientImportModal.vue'
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

const { t } = useI18n({ useScope: 'global' })

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
  showImportKeyModal,
  selectedImportKey,
  revealingKeys,
  openAiBaseUrl,
  showCreatedKey,
  clearCreatedKey,
  copyToClipboard,
  copyApiKey,
  importCreatedKey,
  openUseKeyModal,
  openImportKeyModal,
  importSelectedKey,
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
    <ZPageHeader
      :title="t('navigation.apiKeys')"
      :description="t('keys.page.description')"
    />

    <ZCard
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
          <ZTable
            class="min-h-0 flex-1"
            :columns="apiKeyColumns"
            :data="apiKeys"
            :loading="loading"
            :selected-row-keys="selectedRowKeys"
            :sort="sort"
            :empty-text="t('keys.page.noApiKeyYet')"
            @sort-change="handleSortChange"
          >
            <template #header-selection>
              <ZCheckbox
                :model-value="allSelected"
                :indeterminate="indeterminate"
                :aria-label="t('keys.page.selectCurrentPageKey')"
                @update:model-value="toggleAll"
              />
            </template>
            <template #selection="{ row }">
              <ZCheckbox
                :model-value="selectedIds.has(row.id)"
                :aria-label="t('keys.page.selectKey')"
                @update:model-value="toggleSelection(row.id)"
              />
            </template>
            <template #identity="{ row }">
              <ApiKeyIdentityCell :api-key="row" />
            </template>
            <template #prefix="{ row }">
              <ApiKeyPrefixCell
                :prefix="row.prefix"
                :revealing="revealingKeys.has(row.id)"
                :copying="revealingKeys.get(row.id) === 'copy'"
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
              <dl class="m-0 grid grid-cols-[max-content_max-content] gap-x-2 gap-y-1 text-xs tabular-nums">
                <dt class="text-right text-z-text-tertiary">
                  {{ t('keys.page.concurrency') }}
                </dt>
                <dd class="m-0 whitespace-nowrap text-z-text">
                  {{ row.maxConcurrency || '∞' }}
                </dd>
                <dt class="text-right text-z-text-tertiary">
                  RPM
                </dt>
                <dd class="m-0 whitespace-nowrap text-z-text">
                  {{ row.requestsPerMinute || '∞' }}
                </dd>
              </dl>
            </template>
            <template #enabled="{ row }">
              <ApiKeyStatusBadge :api-key="row" />
            </template>
            <template #lastUsedAt="{ row }">
              <LastUsedAtCell :value="row.lastUsedAt" :full-display="row.lastUsedAtFullDisplay" />
            </template>
            <template #actions="{ row }">
              <ApiKeyActions
                :api-key="row"
                :deleting="deletingKey"
                :revealing="revealingKeys.has(row.id)"
                :configuring="revealingKeys.get(row.id) === 'use'"
                :updating-status="updatingStatusKeyIds.has(row.id)"
                @edit="openEdit"
                @reset-budget="openBudgetReset"
                @delete="requestDeleteKey"
                @import-client="openImportKeyModal"
                @toggle="handleToggleStatus"
                @use="openUseKeyModal"
              />
            </template>
          </ZTable>
          <ZPagination
            v-bind="apiKeyPagination"
            :disabled="loading"
            @current-change="handlePageChange"
            @size-change="handlePageSizeChange"
          />
        </div>
      </template>
    </ZCard>

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
      @import-client="importCreatedKey"
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

    <ClientImportModal
      v-model="showImportKeyModal"
      @select="importSelectedKey"
      @after-leave="selectedImportKey = null"
    />

    <ZConfirmDialog
      v-model="showAllAccountsConfirm"
      :title="t('keys.scope.allAccounts.title')"
      :description="t('keys.scope.allAccounts.description')"
      :confirm-text="t('keys.scope.allAccounts.confirm')"
      :loading="savingKey"
      @confirm="confirmAllAccountsScope"
    />

    <ZConfirmDialog
      v-model="showDeleteModal"
      :title="t('common.actions.delete')"
      :description="t('keys.page.batchDeleteWarning')"
      destructive
      :confirm-text="t('common.actions.delete')"
      :loading="batchDeleting"
      @confirm="handleBatchDelete"
    >
      <p class="m-0">
        {{ t('keys.page.batchDeleteDescription', { count: deleteCount }) }}
      </p>
    </ZConfirmDialog>

    <ZConfirmDialog
      v-model="showSingleDeleteModal"
      :title="t('keys.page.deleteApiKey')"
      :description="t('keys.page.deleteWarning')"
      destructive
      :confirm-text="t('common.actions.delete')"
      :loading="deletingKey"
      @confirm="handleDelete"
    >
      <p class="m-0">
        {{ t('keys.page.delete', { name: pendingDeleteKey?.name || pendingDeleteKey?.prefix || t('keys.page.apiKey') }) }}
      </p>
    </ZConfirmDialog>
  </div>
</template>
