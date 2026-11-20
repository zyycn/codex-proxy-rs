<script setup lang="ts">
import type { FastMode } from '@/api'
import { ZCard, ZCheckbox, ZConfirmDialog, ZPageHeader, ZPagination, ZTable } from '@codex-proxy/ui'

import { ImageOff, Zap, ZapOff } from '@lucide/vue'
import { useI18n } from 'vue-i18n'
import { usePageSelection } from '@/composables/usePageSelection'
import AccountGroupActions from './components/AccountGroupActions.vue'
import AccountGroupFilters from './components/AccountGroupFilters.vue'
import AccountGroupFormModal from './components/AccountGroupFormModal.vue'
import AccountGroupMetricsCell from './components/AccountGroupMetricsCell.vue'
import { useAccountGroups } from './composables/useAccountGroups'
import { accountGroupColumns } from './constants'

const { t } = useI18n({ useScope: 'global' })

function fastModeLabel(mode: FastMode) {
  if (mode === 'disabled')
    return t('groups.page.fastModeIsOff')
  return mode === 'ultrafast'
    ? t('groups.editor.forceUltrafast')
    : t('groups.editor.forceFast')
}

const {
  groups,
  loading,
  pagination,
  searchQuery,
  showFormModal,
  showDeleteModal,
  showBatchDeleteModal,
  showDisableModal,
  selectedIds,
  editingGroup,
  pendingDeleteGroup,
  pendingDisableGroup,
  deleteCount,
  form,
  saving,
  deleting,
  batchDeleting,
  disabling,
  updatingStatusGroupIds,
  referencedKeyNames,
  openCreate,
  openEdit,
  save,
  requestToggle,
  confirmDisable,
  requestDelete,
  confirmDelete,
  confirmBatchDelete,
  handlePageChange,
  handlePageSizeChange,
} = useAccountGroups()

const { allSelected, indeterminate, selectedRowKeys, toggleSelection, toggleAll }
  = usePageSelection(groups, selectedIds)
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col overflow-hidden">
    <ZPageHeader
      :title="t('navigation.groups')"
      :description="t('groups.page.description')"
    />

    <ZCard
      class="mt-5 flex h-[calc(100dvh-136px)] min-h-125 flex-col"
    >
      <template #header>
        <AccountGroupFilters
          v-model:search="searchQuery"
          :batch-deleting="batchDeleting"
          :selected-count="selectedIds.size"
          @create="openCreate"
          @delete-selected="showBatchDeleteModal = true"
        />
      </template>

      <template #body>
        <div class="flex h-full min-h-0 flex-col">
          <ZTable
            class="min-h-0 flex-1"
            :columns="accountGroupColumns"
            :data="groups"
            :loading="loading"
            :selected-row-keys="selectedRowKeys"
            :empty-text="t('groups.page.empty')"
          >
            <template #header-selection>
              <ZCheckbox
                :model-value="allSelected"
                :indeterminate="indeterminate"
                :aria-label="t('groups.page.selectCurrentPageGroup')"
                @update:model-value="toggleAll"
              />
            </template>
            <template #selection="{ row }">
              <ZCheckbox
                :model-value="selectedIds.has(row.id)"
                :aria-label="t('groups.page.selectGroup')"
                @update:model-value="toggleSelection(row.id)"
              />
            </template>
            <template #identity="{ row }">
              <div class="grid min-w-0 gap-1">
                <div class="flex min-w-0 flex-wrap items-center gap-2">
                  <strong class="truncate text-z text-z-text">
                    {{ row.name }}
                  </strong>
                  <span
                    v-if="row.fastMode !== 'default'"
                    class="inline-flex h-4 shrink-0 items-center rounded-full px-0.5"
                    :class="row.fastMode !== 'disabled'
                      ? 'bg-z-warning-container text-z-warning-on-container'
                      : 'bg-z-fill-tertiary text-z-text-tertiary'"
                    :title="fastModeLabel(row.fastMode)"
                    role="img"
                    :aria-label="fastModeLabel(row.fastMode)"
                  >
                    <Zap v-if="row.fastMode !== 'disabled'" class="size-3" stroke-width="2.2" aria-hidden="true" />
                    <ZapOff v-else class="size-3" stroke-width="2.2" aria-hidden="true" />
                    <Zap v-if="row.fastMode === 'ultrafast'" class="size-3" stroke-width="2.2" aria-hidden="true" />
                  </span>
                  <span
                    v-if="!row.allowImageGeneration"
                    class="inline-flex size-4 shrink-0 items-center justify-center rounded-full bg-z-fill-tertiary text-z-text-tertiary"
                    :title="t('groups.page.imageGenerationDisabled')"
                    role="img"
                    :aria-label="t('groups.page.imageGenerationDisabled')"
                  >
                    <ImageOff class="size-3" stroke-width="2.2" aria-hidden="true" />
                  </span>
                </div>
                <span v-if="row.description" class="truncate text-z-xs font-emphasis text-z-text-quaternary">
                  {{ row.description }}
                </span>
              </div>
            </template>

            <template #color="{ row }">
              <span
                class="mx-auto block size-4 rounded-sm"
                :style="{ backgroundColor: row.color }"
                :title="row.color"
                :aria-label="t('groups.page.colorLabel', { color: row.color })"
              />
            </template>

            <template #enabled="{ row }">
              <span
                class="inline-flex h-6 items-center rounded-lg px-2 text-z-xs font-bold"
                :class="row.enabled
                  ? 'bg-z-success-container text-z-success-on-container'
                  : 'bg-z-fill-tertiary text-z-text-quaternary'"
              >
                {{ row.enabled ? t('common.status.enabled') : t('common.status.disabled') }}
              </span>
            </template>

            <template #accountCount="{ row }">
              <AccountGroupMetricsCell :group="row" kind="accounts" />
            </template>

            <template #capacity="{ row }">
              <AccountGroupMetricsCell :group="row" kind="capacity" />
            </template>

            <template #usage="{ row }">
              <AccountGroupMetricsCell :group="row" kind="usage" />
            </template>

            <template #actions="{ row }">
              <AccountGroupActions
                :group="row"
                :deleting="deleting"
                :updating-status="updatingStatusGroupIds.has(row.id)"
                @edit="openEdit"
                @toggle="requestToggle"
                @delete="requestDelete"
              />
            </template>
          </ZTable>
          <ZPagination
            v-bind="pagination"
            :disabled="loading"
            @current-change="handlePageChange"
            @size-change="handlePageSizeChange"
          />
        </div>
      </template>
    </ZCard>

    <AccountGroupFormModal
      v-model="showFormModal"
      v-model:form="form"
      :group="editingGroup"
      :saving="saving"
      @save="save"
    />

    <ZConfirmDialog
      v-model="showBatchDeleteModal"
      :title="t('common.actions.delete')"
      :description="t('groups.page.batchDeleteWarning')"
      destructive
      :confirm-text="t('common.actions.delete')"
      :loading="batchDeleting"
      @confirm="confirmBatchDelete"
    >
      <p class="m-0">
        {{ t('groups.page.batchDeleteDescription', { count: deleteCount }) }}
      </p>
    </ZConfirmDialog>

    <ZConfirmDialog
      v-model="showDisableModal"
      :title="t('groups.page.disableAccountGrouping')"
      :description="t('groups.page.disableDescription')"
      :confirm-text="t('groups.page.confirmDisable')"
      :loading="disabling"
      @confirm="confirmDisable"
    >
      <p class="m-0">
        {{ t('groups.page.disable', { name: pendingDisableGroup?.name }) }}
      </p>
      <p v-if="referencedKeyNames.length > 0" class="mt-2 mb-0 text-z-warning-text">
        {{ t('groups.page.affectedApiKeys', { count: referencedKeyNames.length, value: referencedKeyNames.join('、') }) }}
      </p>
      <p v-else-if="pendingDisableGroup?.clientKeyCount" class="mt-2 mb-0 text-z-warning-text">
        {{ t('groups.page.affectedKeys', { count: pendingDisableGroup.clientKeyCount }) }}
      </p>
    </ZConfirmDialog>

    <ZConfirmDialog
      v-model="showDeleteModal"
      :title="t('groups.page.deleteAccountGroup')"
      :description="t('groups.page.deleteWarning')"
      destructive
      :confirm-text="t('common.actions.delete')"
      :loading="deleting"
      @confirm="confirmDelete"
    >
      <p class="m-0">
        {{ t('groups.page.deleteAccountsWillBeKept', { name: pendingDeleteGroup?.name || t('groups.page.unnamedGroup') }) }}
      </p>
    </ZConfirmDialog>
  </div>
</template>
