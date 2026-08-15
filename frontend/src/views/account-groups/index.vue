<script setup lang="ts">
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { usePageSelection } from '@/composables/usePageSelection'
import AccountGroupActions from './components/AccountGroupActions.vue'
import AccountGroupFilters from './components/AccountGroupFilters.vue'
import AccountGroupFormModal from './components/AccountGroupFormModal.vue'
import AccountGroupMetricsCell from './components/AccountGroupMetricsCell.vue'
import { useAccountGroups } from './composables/useAccountGroups'
import { accountGroupColumns } from './constants'

const {
  groups,
  loading,
  pagination,
  searchQuery,
  statusQuery,
  showFormModal,
  showDeleteModal,
  showBatchDeleteModal,
  showDisableModal,
  selectedIds,
  editingGroup,
  pendingDeleteGroup,
  pendingDisableGroup,
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
    <BasePageHeader
      class="h-17"
      title="分组管理"
      description="将账号归类管理，并为每个 API 密钥指定可使用的账号"
    />

    <BaseCard
      class="mt-5 flex h-[calc(100dvh-136px)] min-h-125 flex-col"
    >
      <template #header>
        <AccountGroupFilters
          v-model:search="searchQuery"
          v-model:status="statusQuery"
          :batch-deleting="batchDeleting"
          :selected-count="selectedIds.size"
          @create="openCreate"
          @delete-selected="showBatchDeleteModal = true"
        />
      </template>

      <template #body>
        <div class="flex h-full min-h-0 flex-col">
          <BaseTable
            class="min-h-0 flex-1"
            :columns="accountGroupColumns"
            :rows="groups"
            :loading="loading"
            :selected-row-keys="selectedRowKeys"
            empty-text="暂无分组，请点击创建分组创建"
          >
            <template #header-selection>
              <BaseCheckbox
                :model-value="allSelected"
                :indeterminate="indeterminate"
                label="选择当前页分组"
                @update:model-value="toggleAll"
              />
            </template>
            <template #selection="{ row }">
              <BaseCheckbox
                :model-value="selectedIds.has(row.id)"
                label="选择分组"
                @update:model-value="toggleSelection(row.id)"
              />
            </template>
            <template #identity="{ row }">
              <div class="grid min-w-0 gap-1">
                <strong class="truncate text-[13px] text-cp-primary">
                  {{ row.name }}
                </strong>
                <span class="truncate text-[11px] font-emphasis text-cp-muted-text">
                  {{ row.description || '未填写描述' }}
                </span>
              </div>
            </template>

            <template #color="{ row }">
              <span
                class="mx-auto block size-4 rounded-sm"
                :style="{ backgroundColor: row.color }"
                :title="row.color"
                :aria-label="`分组颜色 ${row.color}`"
              />
            </template>

            <template #enabled="{ row }">
              <span
                class="inline-flex h-6 items-center rounded-lg px-2 text-[11px] font-bold"
                :class="row.enabled
                  ? 'bg-cp-success-bg text-cp-success-text'
                  : 'bg-cp-muted text-cp-muted-text'"
              >
                {{ row.enabled ? '已启用' : '已禁用' }}
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
          </BaseTable>
          <BaseTablePagination
            :pagination="pagination"
            :loading="loading"
            @page-change="handlePageChange"
            @page-size-change="handlePageSizeChange"
          />
        </div>
      </template>
    </BaseCard>

    <AccountGroupFormModal
      v-model="showFormModal"
      v-model:form="form"
      :group="editingGroup"
      :saving="saving"
      @save="save"
    />

    <BaseConfirmModal
      v-model="showBatchDeleteModal"
      title="确认删除"
      description="删除后这些分组将立即失效，此操作不可撤销"
      destructive
      confirm-text="确认删除"
      :loading="batchDeleting"
      @confirm="confirmBatchDelete"
    >
      <p class="m-0">
        确定删除选中的 {{ selectedIds.size }} 个分组吗？账号本身不会被删除。
      </p>
    </BaseConfirmModal>

    <BaseConfirmModal
      v-model="showDisableModal"
      title="禁用账号分组"
      description="禁用后，使用该分组的 API 密钥将无法再使用其中的账号。"
      confirm-text="确认禁用"
      :loading="disabling"
      @confirm="confirmDisable"
    >
      <p class="m-0">
        确定禁用“{{ pendingDisableGroup?.name }}”吗？
      </p>
      <p v-if="referencedKeyNames.length > 0" class="mt-2 mb-0 text-cp-warning-text">
        将影响 {{ referencedKeyNames.length }} 个 API 密钥：{{ referencedKeyNames.join('、') }}
      </p>
      <p v-else-if="pendingDisableGroup?.clientKeyCount" class="mt-2 mb-0 text-cp-warning-text">
        将影响 {{ pendingDisableGroup.clientKeyCount }} 个 API 密钥。
      </p>
    </BaseConfirmModal>

    <BaseConfirmModal
      v-model="showDeleteModal"
      title="删除账号分组"
      description="删除后该分组将立即失效，此操作不可撤销"
      destructive
      confirm-text="确认删除"
      :loading="deleting"
      @confirm="confirmDelete"
    >
      <p class="m-0">
        确定删除“{{ pendingDeleteGroup?.name || '该分组' }}”吗？账号本身不会被删除。
      </p>
    </BaseConfirmModal>
  </div>
</template>
