<script setup lang="ts">
import { ChevronDown } from '@lucide/vue'
import { ref } from 'vue'

import AccountGroupMarks from '@/components/AccountGroupMarks.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import LastUsedAtCell from '@/components/LastUsedAtCell.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { useAccountGroupCatalog } from '@/composables/useAccountGroupCatalog'
import AccountBatchEditModal from './components/AccountBatchEditModal.vue'
import AccountConnectionTestModal from './components/AccountConnectionTestModal.vue'
import AccountCreateModal from './components/AccountCreateModal.vue'
import AccountEditModal from './components/AccountEditModal.vue'
import AccountFilters from './components/AccountFilters.vue'
import AccountIdentityCell from './components/AccountIdentityCell.vue'
import AccountOverviewCards from './components/AccountOverviewCards.vue'
import AccountPlanBadge from './components/AccountPlanBadge.vue'
import AccountQuotaPanel from './components/AccountQuotaPanel.vue'
import AccountQuotaSummaryCell from './components/AccountQuotaSummaryCell.vue'
import AccountStatusBadge from './components/AccountStatusBadge.vue'
import AccountTableActions from './components/AccountTableActions.vue'
import AccountUsagePanel from './components/AccountUsagePanel.vue'
import { useAccountBatchEditor } from './composables/useAccountBatchEditor'
import { useAccountConnectionTest } from './composables/useAccountConnectionTest'
import { useAccountEditor } from './composables/useAccountEditor'
import { useAccountMutations } from './composables/useAccountMutations'
import { useAccountsQuery } from './composables/useAccountsQuery'
import { useAccountsTable } from './composables/useAccountsTable'
import { accountColumns, derivedAccountStatus } from './constants'

const selectedIds = ref<Set<string>>(new Set())
const {
  loading,
  accounts,
  loadAccounts,
  refreshAccountsSilently,
  searchQuery,
  providerQuery,
  statusQuery,
  groupQuery,
  sort,
  accountSummary,
  accountPagination,
  replaceAccount,
  handlePageChange,
  handlePageSizeChange,
  handleSortChange,
} = useAccountsQuery()

const {
  groups,
  loading: groupsLoading,
  loadGroups,
} = useAccountGroupCatalog()

const {
  showCreateModal,
  showDeleteModal,
  showSingleDeleteModal,
  pendingDeleteAccount,
  recoveringAccountIds,
  refreshingAccountIds,
  refreshingQuotaAccountIds,
  deletingAccount,
  creatingAccount,
  authorizingOAuth,
  batchDeleting,
  exportingAccounts,
  reauthorizingAccount,
  createForm,
  handleCreate,
  handleAuthorizeOAuth,
  openCreateAccount,
  openReauthorizeAccount,
  requestDeleteAccount,
  handleDelete,
  handleBatchDelete,
  handleExportAccounts,
  handleRecover,
  handleRefresh,
  handleRefreshQuota,
} = useAccountMutations({
  accounts,
  selectedIds,
  reload: loadAccounts,
  replaceAccount,
})

const {
  showConnectionTestModal,
  testingAccount,
  connectionTestStatus,
  connectionTestModel,
  connectionTestLogs,
  connectionTestError,
  connectionTestStartedAt,
  connectionTestFinishedAt,
  connectionTestDurationMs,
  testingConnectionIds,
  loadingConnectionTestModels,
  refreshingConnectionTestModels,
  connectionTestSelectedModel,
  connectionTestModelOptions,
  connectionTestStatusView,
  openConnectionTest,
  handleRefreshConnectionTestModels,
  handleTestConnection,
} = useAccountConnectionTest({ reload: refreshAccountsSilently })

const {
  expandedAccountIds,
  allSelected,
  indeterminate,
  selectedRowKeys,
  expandedRowKeys,
  toggleSelection,
  toggleExpanded,
  toggleAll,
} = useAccountsTable(accounts, selectedIds)

const {
  showBatchEditModal,
  schedulingEnabled: batchSchedulingEnabled,
  concurrencyLimit: batchConcurrencyLimit,
  weight: batchWeight,
  selectedGroupIds: batchGroupIds,
  saving: savingBatchEdit,
  open: openBatchEdit,
  save: saveBatchEdit,
} = useAccountBatchEditor({
  accounts,
  selectedIds,
  reloadAccounts: loadAccounts,
  reloadGroups: loadGroups,
})

const {
  showEditModal,
  editingAccount,
  schedulingEnabled,
  concurrencyLimit: editingConcurrencyLimit,
  weight: editingWeight,
  selectedGroupIds: editingGroupIds,
  saving: savingAccountEdit,
  open: openAccountEdit,
  save: saveAccountEdit,
} = useAccountEditor({
  accounts,
  reloadAccounts: loadAccounts,
  reloadGroups: loadGroups,
})
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col overflow-hidden">
    <BasePageHeader
      class="h-17"
      title="账号管理"
      description="维护账号池，查看可用性、配额与使用状态"
    />

    <AccountOverviewCards :summary="accountSummary" />

    <BaseCard
      class="mt-4 flex h-[calc(100dvh-250px)] min-h-125 flex-col"
    >
      <template #header>
        <AccountFilters
          v-model:search="searchQuery"
          v-model:status="statusQuery"
          v-model:provider="providerQuery"
          v-model:group="groupQuery"
          :groups="groups"
          :groups-loading="groupsLoading"
          :selected-count="selectedIds.size"
          :batch-deleting="batchDeleting"
          :exporting-accounts="exportingAccounts"
          @delete-selected="showDeleteModal = true"
          @export-selected="handleExportAccounts"
          @create="openCreateAccount"
          @edit-selected="openBatchEdit"
        />
      </template>

      <template #body>
        <div class="flex h-full min-h-0 flex-col">
          <BaseTable
            class="min-h-0 flex-1 [--cp-table-row-min-height:72px]"
            :columns="accountColumns"
            :rows="accounts"
            :loading="loading"
            :selected-row-keys="selectedRowKeys"
            :expanded-row-keys="expandedRowKeys"
            :sort="sort"
            empty-text="暂无账号数据"
            @sort-change="handleSortChange"
          >
            <template #expander="{ row }">
              <button
                type="button"
                class="inline-flex size-6 cursor-pointer items-center justify-center rounded-md border-0 bg-transparent text-cp-secondary transition hover:bg-cp-default-hover hover:text-cp-primary"
                :title="expandedAccountIds.has(row.id) ? '收起统计' : '展开统计'"
                @click.stop="toggleExpanded(row.id)"
              >
                <ChevronDown
                  class="size-3.5 transition-transform"
                  :class="expandedAccountIds.has(row.id) ? '' : '-rotate-90'"
                />
              </button>
            </template>

            <template #header-selection>
              <BaseCheckbox
                :model-value="allSelected"
                :indeterminate="indeterminate"
                label="选择当前页账号"
                @update:model-value="toggleAll"
              />
            </template>

            <template #selection="{ row }">
              <BaseCheckbox
                :model-value="selectedIds.has(row.id)"
                label="选择账号"
                @update:model-value="toggleSelection(row.id)"
              />
            </template>

            <template #identity="{ row }">
              <AccountIdentityCell :account="row" />
            </template>

            <template #provider="{ row }">
              <ProviderIconGroup
                :provider="row.provider"
                :authentication-kind="row.authenticationKind"
              />
            </template>

            <template #status="{ row }">
              <AccountStatusBadge
                :status="derivedAccountStatus(row)"
                :error-reason="row.errorReason"
                :error-message="row.errorMessage"
                :rate-limited-until="row.quota.rateLimitedUntil"
              />
            </template>

            <template #planType="{ row }">
              <AccountPlanBadge :plan-type="row.planType" />
            </template>

            <template #usage="{ row }">
              <AccountQuotaSummaryCell :account="row" />
            </template>

            <template #groups="{ row }">
              <div class="flex w-full justify-center">
                <AccountGroupMarks :groups="row.groups" />
              </div>
            </template>

            <template #lastUsedAt="{ row }">
              <LastUsedAtCell :value="row.usage.lastUsedAt" />
            </template>

            <template #actions="{ row }">
              <AccountTableActions
                :account="row"
                :deleting="deletingAccount"
                :recovering="recoveringAccountIds.has(row.id)"
                :refreshing="refreshingAccountIds.has(row.id)"
                :testing="testingConnectionIds.has(row.id)"
                @edit="openAccountEdit"
                @delete="requestDeleteAccount"
                @recover="handleRecover"
                @refresh="handleRefresh"
                @reauthorize="openReauthorizeAccount"
                @test="openConnectionTest"
              />
            </template>

            <template #expanded="{ row }">
              <div class="grid gap-3 p-4 lg:grid-cols-[1.05fr_2.45fr]">
                <AccountQuotaPanel
                  :account="row"
                  :refreshing="refreshingQuotaAccountIds.has(row.id)"
                  @refresh-quota="handleRefreshQuota"
                />
                <AccountUsagePanel :account="row" />
              </div>
            </template>
          </BaseTable>
          <BaseTablePagination
            :pagination="accountPagination"
            :loading="loading"
            @page-change="handlePageChange"
            @page-size-change="handlePageSizeChange"
          />
        </div>
      </template>
    </BaseCard>

    <AccountConnectionTestModal
      v-model="showConnectionTestModal"
      v-model:selected-model="connectionTestSelectedModel"
      :account="testingAccount"
      :duration-ms="connectionTestDurationMs"
      :error="connectionTestError"
      :finished-at="connectionTestFinishedAt"
      :loading-models="loadingConnectionTestModels"
      :refreshing-models="refreshingConnectionTestModels"
      :logs="connectionTestLogs"
      :model="connectionTestModel"
      :model-options="connectionTestModelOptions"
      :started-at="connectionTestStartedAt"
      :status="connectionTestStatus"
      :status-view="connectionTestStatusView"
      @refresh-models="handleRefreshConnectionTestModels()"
      @test="handleTestConnection()"
    />

    <AccountCreateModal
      v-model="showCreateModal"
      v-model:form="createForm"
      :account="reauthorizingAccount"
      :oauth-loading="authorizingOAuth"
      :reauthorizing="Boolean(reauthorizingAccount)"
      :saving="creatingAccount"
      @create="handleCreate"
      @generate-oauth="handleAuthorizeOAuth"
    />

    <AccountEditModal
      v-model="showEditModal"
      v-model:enabled="schedulingEnabled"
      v-model:concurrency-limit="editingConcurrencyLimit"
      v-model:weight="editingWeight"
      v-model:selected-group-ids="editingGroupIds"
      :account="editingAccount"
      :groups="groups"
      :groups-loading="groupsLoading"
      :saving="savingAccountEdit"
      @save="saveAccountEdit"
    />

    <AccountBatchEditModal
      v-model="showBatchEditModal"
      v-model:enabled="batchSchedulingEnabled"
      v-model:concurrency-limit="batchConcurrencyLimit"
      v-model:weight="batchWeight"
      v-model:selected-group-ids="batchGroupIds"
      :selected-count="selectedIds.size"
      :groups="groups"
      :groups-loading="groupsLoading"
      :saving="savingBatchEdit"
      @save="saveBatchEdit"
    />

    <BaseConfirmModal
      v-model="showDeleteModal"
      title="确认删除"
      description="删除后该账号将不再参与调度，此操作不可撤销"
      destructive
      confirm-text="确认删除"
      :loading="batchDeleting"
      @confirm="handleBatchDelete"
    >
      <p class="m-0">
        确定要删除选中的 {{ selectedIds.size }} 个账号吗？此操作不可撤销
      </p>
    </BaseConfirmModal>

    <BaseConfirmModal
      v-model="showSingleDeleteModal"
      title="删除账号"
      description="删除后该账号将不再参与调度，此操作不可撤销"
      destructive
      confirm-text="确认删除"
      :loading="deletingAccount"
      @confirm="handleDelete"
    >
      <p class="m-0">
        确定要删除
        {{
          pendingDeleteAccount?.email
            || pendingDeleteAccount?.accountId
            || pendingDeleteAccount?.id
            || '该账号'
        }}
        吗？
      </p>
    </BaseConfirmModal>
  </div>
</template>
