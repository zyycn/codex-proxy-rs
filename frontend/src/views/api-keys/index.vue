<script setup lang="ts">
import { computed, ref, shallowRef } from 'vue'

import { API_BASE_URL } from '@/api/constants'
import type { ClientApiKey } from '@/api/modules/api-keys'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { apiKeyColumns } from './constants'
import { useApiKeyFilters } from './composables/useApiKeyFilters'
import { useApiKeyMutations } from './composables/useApiKeyMutations'
import { useApiKeysTable } from './composables/useApiKeysTable'
import { buildCodexCcSwitchImportDeeplink } from './utils/ccswitchImport'
import ApiKeyActions from './components/ApiKeyActions.vue'
import ApiKeyCreateModal from './components/ApiKeyCreateModal.vue'
import ApiKeyFilters from './components/ApiKeyFilters.vue'
import ApiKeyIdentityCell from './components/ApiKeyIdentityCell.vue'
import ApiKeyPrefixCell from './components/ApiKeyPrefixCell.vue'
import ApiKeyStatusBadge from './components/ApiKeyStatusBadge.vue'
import ApiKeyUseModal from './components/ApiKeyUseModal.vue'

const selectedIds = ref<Set<string>>(new Set())
const totalApiKeys = ref(0)
const showUseKeyModal = shallowRef(false)
const selectedUseKey = shallowRef<ClientApiKey | null>(null)

const {
  page,
  pageSize,
  searchQuery,
  sort,
  apiKeyPagination,
  bindApiKeyLoader,
  handlePageChange,
  handlePageSizeChange,
  handleSortChange,
} = useApiKeyFilters(totalApiKeys)

const {
  loading,
  apiKeys,
  showCreateModal,
  showDeleteModal,
  showSingleDeleteModal,
  showKeyModal,
  createdKey,
  createdKeyName,
  pendingDeleteKey,
  creatingKey,
  deletingKey,
  batchDeleting,
  updatingStatusKeyIds,
  createForm,
  loadApiKeys,
  handleCreate,
  requestDeleteKey,
  handleDelete,
  handleBatchDelete,
  handleToggleStatus,
  copyToClipboard,
} = useApiKeyMutations({ page, pageSize, searchQuery, sort, selectedIds, totalApiKeys })

const { allSelected, indeterminate, selectedRowKeys, toggleSelection, toggleAll } = useApiKeysTable(
  apiKeys,
  selectedIds,
)

bindApiKeyLoader(loadApiKeys)

const serviceRootUrl = computed(() => resolveServiceRootUrl())
const openAiBaseUrl = computed(() => `${serviceRootUrl.value}/v1`)

function resolveServiceRootUrl() {
  const normalizedApiBase = API_BASE_URL.trim().replace(/\/+$/, '')

  if (/^https?:\/\//i.test(normalizedApiBase)) {
    return normalizedApiBase
  }

  if (typeof window === 'undefined') {
    return normalizedApiBase
  }

  const origin = window.location.origin.replace(/\/+$/, '')
  if (!normalizedApiBase) {
    return origin
  }

  return `${origin}${normalizedApiBase.startsWith('/') ? normalizedApiBase : `/${normalizedApiBase}`}`
}

function importCreatedKeyToCcs() {
  if (!createdKey.value) return

  window.location.href = buildCodexCcSwitchImportDeeplink({
    apiKey: createdKey.value,
    baseUrl: openAiBaseUrl.value,
    providerName: createdKeyName.value || 'codex-proxy-rs',
  })
}

function openUseKeyModal(apiKey: ClientApiKey) {
  selectedUseKey.value = apiKey
  showUseKeyModal.value = true
}

function importToCcs(apiKey: ClientApiKey) {
  if (!apiKey.key) return

  window.location.href = buildCodexCcSwitchImportDeeplink({
    apiKey: apiKey.key,
    baseUrl: openAiBaseUrl.value,
    providerName: apiKey.name || apiKey.prefix || 'codex-proxy-rs',
  })
}
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col overflow-hidden">
    <header class="flex h-17 shrink-0 items-start justify-between">
      <div>
        <h1 class="mt-0 mb-0 text-[34px] leading-[1.15] font-extrabold text-(--cp-text-primary)">
          API 密钥
        </h1>
        <p class="mt-2.5 mb-0 text-[15px] leading-[1.15] font-semibold text-(--cp-text-secondary)">
          签发与维护客户端访问凭证，控制网关调用入口
        </p>
      </div>
    </header>

    <BaseCard
      :padded="false"
      class="mt-5 flex h-[calc(100dvh-136px)] min-h-125 flex-col"
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
          :rows="apiKeys"
          :loading="loading"
          :selected-row-keys="selectedRowKeys"
          :pagination="apiKeyPagination"
          :sort="sort"
          empty-text="暂无 API Key"
          min-width="1320px"
          @page-change="handlePageChange"
          @page-size-change="handlePageSizeChange"
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
            <ApiKeyPrefixCell :key-value="row.key" :prefix="row.prefix" @copy="copyToClipboard" />
          </template>

          <template #enabled="{ row }">
            <ApiKeyStatusBadge :api-key="row" />
          </template>

          <template #actions="{ row }">
            <ApiKeyActions
              :api-key="row"
              :deleting="deletingKey"
              :updating-status="updatingStatusKeyIds.has(row.id)"
              @delete="requestDeleteKey"
              @import-ccs="importToCcs"
              @toggle="handleToggleStatus"
              @use="openUseKeyModal"
            />
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
      @import-ccs="importCreatedKeyToCcs"
    />

    <ApiKeyUseModal
      v-model="showUseKeyModal"
      :api-key="selectedUseKey"
      :api-base-url="openAiBaseUrl"
      @copy="copyToClipboard"
    />

    <BaseConfirmModal
      v-model="showDeleteModal"
      title="确认删除"
      description="删除后这些 API Key 将立即失效，此操作不可撤销"
      variant="danger"
      confirm-text="确认删除"
      :loading="batchDeleting"
      width="480px"
      @confirm="handleBatchDelete"
    >
      <p class="m-0">确定要删除选中的 {{ selectedIds.size }} 个 API Key 吗？此操作不可撤销</p>
    </BaseConfirmModal>

    <BaseConfirmModal
      v-model="showSingleDeleteModal"
      title="删除 API Key"
      description="删除后该 API Key 将立即失效，此操作不可撤销"
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
