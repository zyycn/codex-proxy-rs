<script setup lang="ts">
import { computed, ref, shallowRef, watch } from 'vue'

import { API_BASE_URL } from '@/api/constants'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { toast } from '@/components/base/BaseToast'
import ProviderBadge from '@/components/ProviderBadge.vue'
import { usePageSelection } from '@/composables/usePageSelection'
import { errorMessage } from '@/utils/async'
import ApiKeyActions from './components/ApiKeyActions.vue'
import ApiKeyCreateModal from './components/ApiKeyCreateModal.vue'
import ApiKeyFilters from './components/ApiKeyFilters.vue'
import ApiKeyIdentityCell from './components/ApiKeyIdentityCell.vue'
import ApiKeyPrefixCell from './components/ApiKeyPrefixCell.vue'
import ApiKeyStatusBadge from './components/ApiKeyStatusBadge.vue'
import ApiKeyUseModal from './components/ApiKeyUseModal.vue'
import { useApiKeyMutations } from './composables/useApiKeyMutations'
import { useApiKeysQuery } from './composables/useApiKeysQuery'
import { apiKeyColumns } from './constants'
import { buildCodexCcSwitchImportDeeplink } from './utils/ccswitchImport'

const selectedIds = ref<Set<string>>(new Set())
const showUseKeyModal = shallowRef(false)

const {
  loading,
  apiKeys,
  loadApiKeys,
  configRevision,
  searchQuery,
  sort,
  apiKeyPagination,
  handlePageChange,
  handlePageSizeChange,
  handleSortChange,
} = useApiKeysQuery()

const {
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
  revealingKeyIds,
  createForm,
  handleCreate,
  requestDeleteKey,
  handleDelete,
  handleBatchDelete,
  handleToggleStatus,
  copyToClipboard,
  revealPlaintextKey,
  copyApiKey,
} = useApiKeyMutations({ selectedIds, configRevision, reload: loadApiKeys })
const selectedUseKey = shallowRef<(typeof apiKeys.value)[number] | null>(null)

const { allSelected, indeterminate, selectedRowKeys, toggleSelection, toggleAll } = usePageSelection(
  apiKeys,
  selectedIds,
)

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
  if (!createdKey.value)
    return

  window.location.href = buildCodexCcSwitchImportDeeplink({
    apiKey: createdKey.value,
    baseUrl: openAiBaseUrl.value,
    providerName: createdKeyName.value || 'codex-proxy-rs',
  })
}

async function openUseKeyModal(apiKey: (typeof apiKeys.value)[number]) {
  try {
    const key = await revealPlaintextKey(apiKey)
    selectedUseKey.value = { ...apiKey, key }
    showUseKeyModal.value = true
  }
  catch (error: unknown) {
    toast.error(errorMessage(error, '读取完整密钥失败'))
  }
}

async function importToCcs(apiKey: (typeof apiKeys.value)[number]) {
  try {
    const key = await revealPlaintextKey(apiKey)
    window.location.href = buildCodexCcSwitchImportDeeplink({
      apiKey: key,
      baseUrl: openAiBaseUrl.value,
      providerName: apiKey.name || apiKey.prefix || 'codex-proxy-rs',
    })
  }
  catch (error: unknown) {
    toast.error(errorMessage(error, '读取完整密钥失败'))
  }
}

watch(showUseKeyModal, (open) => {
  if (!open)
    selectedUseKey.value = null
})
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col overflow-hidden">
    <BasePageHeader
      class="h-17"
      title="API 密钥"
      description="签发与维护客户端访问凭证，控制网关调用入口"
    />

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
            <ApiKeyPrefixCell
              :prefix="row.prefix"
              :revealing="revealingKeyIds.has(row.id)"
              @copy="copyApiKey(row)"
            />
          </template>

          <template #providerKind="{ row }">
            <ProviderBadge :provider="row.providerKind" />
          </template>

          <template #enabled="{ row }">
            <ApiKeyStatusBadge :api-key="row" />
          </template>

          <template #actions="{ row }">
            <ApiKeyActions
              :api-key="row"
              :deleting="deletingKey"
              :revealing="revealingKeyIds.has(row.id)"
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
      <p class="m-0">
        确定要删除选中的 {{ selectedIds.size }} 个 API Key 吗？此操作不可撤销
      </p>
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
